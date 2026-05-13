//! GitHub provider — PAT-based authentication, viewer info, and the
//! "waiting on me" search across assigned/review-requested/authored/mentioned.
//!
//! M2 deliberately stays close to GitHub's REST search API. GraphQL would
//! consolidate the four queries into one, but the search API works fine for
//! single-digit account counts and avoids hand-rolling a GraphQL client for
//! milestone one of the providers.

use crate::types::{
    CiRun, CiStatus, ItemKind, ItemReason, Provider, Release, Repo, Viewer, WaitingItem,
};
use chrono::{DateTime, Utc};
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use thiserror::Error;

const API_BASE: &str = "https://api.github.com";
const USER_AGENT: &str = concat!("gitBuddy/", env!("CARGO_PKG_VERSION"));
const ACCEPT: &str = "application/vnd.github+json";

#[derive(Debug, Error)]
pub enum GitHubError {
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("authentication failed — check that the token is valid and has `repo` scope")]
    Unauthorized,
    #[error("GitHub API returned HTTP {0}")]
    HttpStatus(StatusCode),
}

pub type Result<T> = std::result::Result<T, GitHubError>;

pub struct GitHubProvider {
    client: Client,
    token: String,
    pub viewer: Viewer,
}

impl GitHubProvider {
    /// Construct a provider from a personal access token, verifying that the
    /// token works by hitting `/user`. Returns the authenticated viewer so
    /// callers can show a "connected as @login" confirmation.
    pub async fn connect(token: String) -> Result<Self> {
        let client = Client::builder().user_agent(USER_AGENT).build()?;
        let viewer = fetch_viewer(&client, &token).await?;
        Ok(Self {
            client,
            token,
            viewer,
        })
    }

    /// Items where the user is assigned, review-requested, authored, or
    /// mentioned. Queries run concurrently; results are deduplicated, since
    /// the same PR can match multiple reasons.
    pub async fn list_waiting(&self) -> Result<Vec<WaitingItem>> {
        let login = self.viewer.login.as_str();
        let queries = [
            (
                ItemReason::Assigned,
                format!("is:open assignee:{login} archived:false"),
            ),
            (
                ItemReason::Review,
                format!("is:open review-requested:{login} archived:false"),
            ),
            (
                ItemReason::Authored,
                format!("is:open author:{login} archived:false"),
            ),
            (
                ItemReason::Mentioned,
                format!("is:open mentions:{login} archived:false"),
            ),
        ];

        let mut handles = Vec::with_capacity(queries.len());
        for (reason, q) in queries {
            let client = self.client.clone();
            let token = self.token.clone();
            handles.push(tokio::spawn(async move {
                search_issues(&client, &token, &q, reason).await
            }));
        }

        let mut items = Vec::new();
        for h in handles {
            match h.await {
                Ok(Ok(mut v)) => items.append(&mut v),
                Ok(Err(e)) => return Err(e),
                // A panic inside a spawned task — surface as a generic error
                // so the UI doesn't silently lose results without explanation.
                Err(_) => return Err(GitHubError::HttpStatus(StatusCode::INTERNAL_SERVER_ERROR)),
            }
        }

        // Dedup by (repo, id) keeping the highest-priority reason.
        items.sort_by(|a, b| {
            a.repo
                .cmp(&b.repo)
                .then(a.id.cmp(&b.id))
                .then(reason_priority(a.reason).cmp(&reason_priority(b.reason)))
        });
        items.dedup_by(|a, b| a.repo == b.repo && a.id == b.id);

        // After dedup, sort by recency (most recently updated first).
        items.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

        Ok(items)
    }

    /// All repos the viewer has explicit access to — owned, collaborator, or
    /// organization member. Paginates through `/user/repos` up to a sane cap
    /// (M2 doesn't need full pagination for an account with thousands of repos
    /// yet; the M5 work will revisit this when multi-account is in).
    pub async fn list_repos(&self) -> Result<Vec<Repo>> {
        let mut all = Vec::new();
        const PAGE_SIZE: u32 = 100;
        const MAX_PAGES: u32 = 5; // 500 repos is plenty for M2

        for page in 1..=MAX_PAGES {
            let resp = self
                .client
                .get(format!("{API_BASE}/user/repos"))
                .bearer_auth(&self.token)
                .header("Accept", ACCEPT)
                .query(&[
                    ("per_page", PAGE_SIZE.to_string()),
                    ("page", page.to_string()),
                    ("sort", "pushed".to_string()),
                    (
                        "affiliation",
                        "owner,collaborator,organization_member".into(),
                    ),
                ])
                .send()
                .await?;

            match resp.status() {
                s if s.is_success() => {}
                StatusCode::UNAUTHORIZED => return Err(GitHubError::Unauthorized),
                s => return Err(GitHubError::HttpStatus(s)),
            }

            let raw: Vec<RawRepo> = resp.json().await?;
            let len = raw.len();
            all.extend(raw.into_iter().map(Into::into));
            if (len as u32) < PAGE_SIZE {
                break;
            }
        }

        Ok(all)
    }

    /// Latest release per repo, for the N most-recently-pushed repos the
    /// viewer has access to. Bounded because /releases/latest is one call
    /// per repo and we don't want to spend the rate-limit budget on dormant
    /// archives.
    pub async fn list_releases(&self) -> Result<Vec<Release>> {
        const MAX_REPOS_TO_CHECK: usize = 60;

        let mut repos = self.list_repos().await?;
        repos.truncate(MAX_REPOS_TO_CHECK);

        let mut handles = Vec::with_capacity(repos.len());
        for repo in repos {
            let client = self.client.clone();
            let token = self.token.clone();
            handles.push(tokio::spawn(async move {
                fetch_latest_release(&client, &token, &repo).await
            }));
        }

        let now = Utc::now();
        let mut releases = Vec::new();
        for h in handles {
            // Skip individual failures rather than fail the whole batch — a
            // single repo erroring out (e.g. abuse-detection 403) shouldn't
            // blank the Releases tab.
            if let Ok(Ok(Some(mut r))) = h.await {
                r.is_new = within_days(&r.published_at, &now, 7);
                r.age_human = humanise_age(&r.published_at, now);
                releases.push(r);
            }
        }

        releases.sort_by(|a, b| b.published_at.cmp(&a.published_at));
        Ok(releases)
    }

    /// Latest CI workflow run on each repo's default branch, for the N most-
    /// recently-pushed repos. Used to paint a coloured status dot next to
    /// each repo row in the UI.
    pub async fn list_ci(&self) -> Result<Vec<CiRun>> {
        const MAX_REPOS_TO_CHECK: usize = 60;

        let mut repos = self.list_repos().await?;
        repos.truncate(MAX_REPOS_TO_CHECK);

        let mut handles = Vec::with_capacity(repos.len());
        for repo in repos {
            let client = self.client.clone();
            let token = self.token.clone();
            handles.push(tokio::spawn(async move {
                fetch_latest_ci_run(&client, &token, &repo).await
            }));
        }

        let mut runs = Vec::new();
        for h in handles {
            if let Ok(Ok(Some(r))) = h.await {
                runs.push(r);
            }
        }
        Ok(runs)
    }
}

async fn fetch_latest_ci_run(client: &Client, token: &str, repo: &Repo) -> Result<Option<CiRun>> {
    let url = format!("{API_BASE}/repos/{}/{}/actions/runs", repo.owner, repo.name);
    let resp = client
        .get(&url)
        .bearer_auth(token)
        .header("Accept", ACCEPT)
        .query(&[("branch", repo.default_branch.as_str()), ("per_page", "1")])
        .send()
        .await?;

    match resp.status() {
        s if s.is_success() => {}
        // Actions not enabled or repo doesn't exist for this branch — emit
        // a "no CI" marker rather than failing the batch.
        StatusCode::NOT_FOUND => {
            return Ok(Some(CiRun {
                repo_id: repo.id.clone(),
                repo_full_name: format!("{}/{}", repo.owner, repo.name),
                status: CiStatus::None,
                html_url: None,
                branch: Some(repo.default_branch.clone()),
                workflow_name: None,
            }));
        }
        StatusCode::UNAUTHORIZED => return Err(GitHubError::Unauthorized),
        // 403 can happen when the repo's owner has disabled actions for
        // forks; treat it the same as "no CI" to keep the batch flowing.
        StatusCode::FORBIDDEN => return Ok(None),
        s => return Err(GitHubError::HttpStatus(s)),
    }

    #[derive(Deserialize)]
    struct WorkflowRunsResp {
        workflow_runs: Vec<WorkflowRun>,
    }
    #[derive(Deserialize)]
    struct WorkflowRun {
        status: String,
        conclusion: Option<String>,
        html_url: String,
        head_branch: Option<String>,
        name: Option<String>,
    }

    let body: WorkflowRunsResp = resp.json().await?;
    let Some(run) = body.workflow_runs.into_iter().next() else {
        return Ok(Some(CiRun {
            repo_id: repo.id.clone(),
            repo_full_name: format!("{}/{}", repo.owner, repo.name),
            status: CiStatus::None,
            html_url: None,
            branch: Some(repo.default_branch.clone()),
            workflow_name: None,
        }));
    };

    Ok(Some(CiRun {
        repo_id: repo.id.clone(),
        repo_full_name: format!("{}/{}", repo.owner, repo.name),
        status: collapse_ci_status(&run.status, run.conclusion.as_deref()),
        html_url: Some(run.html_url),
        branch: run.head_branch,
        workflow_name: run.name,
    }))
}

/// Collapse GitHub's status × conclusion matrix into our four-state enum.
/// `status` is one of queued / in_progress / completed; `conclusion` is only
/// meaningful when status is completed.
fn collapse_ci_status(status: &str, conclusion: Option<&str>) -> CiStatus {
    if status != "completed" {
        return CiStatus::Run;
    }
    match conclusion {
        Some("success") => CiStatus::Ok,
        Some("failure" | "timed_out" | "action_required" | "startup_failure") => CiStatus::Fail,
        Some("cancelled" | "skipped") => CiStatus::Cancelled,
        Some("neutral") => CiStatus::Ok,
        // stale, or some future conclusion value we don't recognise yet
        _ => CiStatus::None,
    }
}

async fn fetch_latest_release(
    client: &Client,
    token: &str,
    repo: &Repo,
) -> Result<Option<Release>> {
    let url = format!(
        "{API_BASE}/repos/{}/{}/releases/latest",
        repo.owner, repo.name
    );
    let resp = client
        .get(&url)
        .bearer_auth(token)
        .header("Accept", ACCEPT)
        .send()
        .await?;

    match resp.status() {
        s if s.is_success() => {}
        // 404 just means "no releases yet" — not an error.
        StatusCode::NOT_FOUND => return Ok(None),
        StatusCode::UNAUTHORIZED => return Err(GitHubError::Unauthorized),
        s => return Err(GitHubError::HttpStatus(s)),
    }

    #[derive(Deserialize)]
    struct RawRelease {
        tag_name: String,
        name: Option<String>,
        html_url: String,
        published_at: Option<String>,
        prerelease: bool,
    }

    let raw: RawRelease = resp.json().await?;
    let Some(published_at) = raw.published_at else {
        return Ok(None);
    };

    let name = raw
        .name
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| raw.tag_name.clone());

    Ok(Some(Release {
        repo_id: repo.id.clone(),
        repo_full_name: format!("{}/{}", repo.owner, repo.name),
        provider: Provider::Github,
        tag: raw.tag_name,
        name,
        published_at,
        html_url: raw.html_url,
        is_prerelease: raw.prerelease,
        is_new: false, // filled in by list_releases against a consistent `now`
        age_human: String::new(),
    }))
}

fn within_days(timestamp: &str, now: &DateTime<Utc>, days: i64) -> bool {
    DateTime::parse_from_rfc3339(timestamp)
        .map(|t| (*now - t.with_timezone(&Utc)).num_days() <= days)
        .unwrap_or(false)
}

#[derive(Deserialize)]
struct RawRepo {
    id: u64,
    name: String,
    owner: RawOwner,
    default_branch: Option<String>,
    language: Option<String>,
    description: Option<String>,
    stargazers_count: u64,
    html_url: String,
    ssh_url: Option<String>,
    clone_url: Option<String>,
    fork: bool,
    private: bool,
    pushed_at: Option<String>,
}

#[derive(Deserialize)]
struct RawOwner {
    login: String,
}

impl From<RawRepo> for Repo {
    fn from(r: RawRepo) -> Self {
        Self {
            id: r.id.to_string(),
            owner: r.owner.login,
            name: r.name,
            provider: Provider::Github,
            default_branch: r.default_branch.unwrap_or_else(|| "main".into()),
            language: r.language,
            description: r.description,
            stars: r.stargazers_count,
            html_url: r.html_url,
            ssh_url: r.ssh_url,
            clone_url: r.clone_url,
            is_fork: r.fork,
            is_private: r.private,
            pushed_at: r.pushed_at,
        }
    }
}

async fn fetch_viewer(client: &Client, token: &str) -> Result<Viewer> {
    let resp = client
        .get(format!("{API_BASE}/user"))
        .bearer_auth(token)
        .header("Accept", ACCEPT)
        .send()
        .await?;

    match resp.status() {
        s if s.is_success() => {
            #[derive(Deserialize)]
            struct Raw {
                login: String,
                avatar_url: Option<String>,
                name: Option<String>,
            }
            let r: Raw = resp.json().await?;
            Ok(Viewer {
                login: r.login,
                avatar_url: r.avatar_url,
                name: r.name,
            })
        }
        StatusCode::UNAUTHORIZED => Err(GitHubError::Unauthorized),
        s => Err(GitHubError::HttpStatus(s)),
    }
}

async fn search_issues(
    client: &Client,
    token: &str,
    q: &str,
    reason: ItemReason,
) -> Result<Vec<WaitingItem>> {
    let resp = client
        .get(format!("{API_BASE}/search/issues"))
        .bearer_auth(token)
        .header("Accept", ACCEPT)
        .query(&[("q", q), ("per_page", "50")])
        .send()
        .await?;

    match resp.status() {
        s if s.is_success() => {}
        StatusCode::UNAUTHORIZED => return Err(GitHubError::Unauthorized),
        s => return Err(GitHubError::HttpStatus(s)),
    }

    #[derive(Deserialize)]
    struct SearchResp {
        items: Vec<RawItem>,
    }
    #[derive(Deserialize)]
    struct RawItem {
        id: u64,
        title: String,
        html_url: String,
        updated_at: String,
        repository_url: String,
        pull_request: Option<serde_json::Value>,
    }

    let body: SearchResp = resp.json().await?;
    let now = Utc::now();

    Ok(body
        .items
        .into_iter()
        .map(|it| WaitingItem {
            id: it.id.to_string(),
            kind: if it.pull_request.is_some() {
                ItemKind::Pr
            } else {
                ItemKind::Is
            },
            title: it.title,
            repo: parse_repo(&it.repository_url),
            provider: Provider::Github,
            reason,
            url: it.html_url,
            age_human: humanise_age(&it.updated_at, now),
            updated_at: it.updated_at,
        })
        .collect())
}

fn parse_repo(api_url: &str) -> String {
    // Inputs look like "https://api.github.com/repos/owner/name"; we want the
    // trailing "owner/name". A naive rsplit on "/repos/" is enough — the
    // value is always provider-controlled.
    api_url
        .rsplit_once("/repos/")
        .map(|(_, tail)| tail.to_string())
        .unwrap_or_else(|| api_url.to_string())
}

fn humanise_age(ts: &str, now: DateTime<Utc>) -> String {
    let Ok(t) = DateTime::parse_from_rfc3339(ts) else {
        return "?".into();
    };
    let delta = now - t.with_timezone(&Utc);
    let mins = delta.num_minutes();
    if mins < 1 {
        "now".into()
    } else if mins < 60 {
        format!("{mins}m")
    } else if mins < 60 * 24 {
        format!("{}h", mins / 60)
    } else if mins < 60 * 24 * 30 {
        format!("{}d", mins / (60 * 24))
    } else if mins < 60 * 24 * 365 {
        format!("{}mo", mins / (60 * 24 * 30))
    } else {
        format!("{}y", mins / (60 * 24 * 365))
    }
}

fn reason_priority(r: ItemReason) -> u8 {
    match r {
        ItemReason::Assigned => 0,
        ItemReason::Review => 1,
        ItemReason::Authored => 2,
        ItemReason::Mentioned => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_repo_from_api_url() {
        assert_eq!(
            parse_repo("https://api.github.com/repos/anthropics/claude-code"),
            "anthropics/claude-code"
        );
    }

    #[test]
    fn humanises_age_buckets() {
        let now = DateTime::parse_from_rfc3339("2026-05-12T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(humanise_age("2026-05-12T11:30:00Z", now), "30m");
        assert_eq!(humanise_age("2026-05-12T08:00:00Z", now), "4h");
        assert_eq!(humanise_age("2026-05-09T12:00:00Z", now), "3d");
    }
}
