//! Gitea-family provider — covers codeberg.org and any self-hosted
//! Forgejo/Gitea instance. Both expose a deliberately GitHub-compatible
//! REST API at `/api/v1/`, so this module mirrors github.rs closely.

use crate::provider_util::{
    collapse_ci_status, http_client, http_error, humanise_age, reason_priority, within_days,
    ProviderBackend, ProviderError,
};
use crate::types::{
    CiRun, CiStatus, ItemKind, ItemReason, Provider, Release, Repo, Viewer, WaitingItem,
};
use chrono::Utc;
use reqwest::{Client, StatusCode};
use serde::Deserialize;

const ACCEPT: &str = "application/json";

/// Hint surfaced when the Gitea/Forgejo API rejects the token.
const AUTH_HINT: &str = "check the token has at least the `read:repository` and `read:user` scopes";

pub type Result<T> = std::result::Result<T, ProviderError>;

pub struct CodebergProvider {
    client: Client,
    token: String,
    base_url: String,
    pub viewer: Viewer,
}

impl CodebergProvider {
    pub async fn connect(token: String, base_url: String) -> Result<Self> {
        let base_url = normalise_base_url(&base_url)?;
        let client = http_client()?;
        let viewer = fetch_viewer(&client, &token, &base_url).await?;
        Ok(Self {
            client,
            token,
            base_url,
            viewer,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Items where the user is assigned / created / review-requested /
    /// mentioned. The four scopes run as parallel calls to
    /// `/repos/issues/search`, then we dedup the way the GitHub provider does.
    pub async fn list_waiting(&self) -> Result<Vec<WaitingItem>> {
        let queries: [(ItemReason, &str); 4] = [
            (ItemReason::Assigned, "assigned"),
            (ItemReason::Authored, "created"),
            (ItemReason::Mentioned, "mentioned"),
            (ItemReason::Review, "review_requested"),
        ];

        let mut handles = Vec::with_capacity(queries.len());
        for (reason, flag) in queries {
            let client = self.client.clone();
            let token = self.token.clone();
            let base = self.base_url.clone();
            handles.push(tokio::spawn(async move {
                search_issues(&client, &token, &base, flag, reason).await
            }));
        }

        let mut items = Vec::new();
        for h in handles {
            match h.await {
                Ok(Ok(mut v)) => items.append(&mut v),
                // Hard auth failures propagate; every other per-scope error
                // (rate limit, transient 5xx, or a panicked task) is tolerated
                // so one failing filter doesn't blank the whole "waiting" list.
                Ok(Err(e @ ProviderError::Unauthorized(_))) => return Err(e),
                Ok(Err(_)) | Err(_) => {}
            }
        }

        items.sort_by(|a, b| {
            a.repo
                .cmp(&b.repo)
                .then(a.id.cmp(&b.id))
                .then(reason_priority(a.reason).cmp(&reason_priority(b.reason)))
        });
        items.dedup_by(|a, b| a.repo == b.repo && a.id == b.id);
        items.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(items)
    }

    pub async fn list_repos(&self) -> Result<Vec<Repo>> {
        let mut all = Vec::new();
        const PAGE_SIZE: u32 = 50;
        const MAX_PAGES: u32 = 5;

        for page in 1..=MAX_PAGES {
            let resp = self
                .client
                .get(format!("{}/api/v1/user/repos", self.base_url))
                .bearer_auth(&self.token)
                .header("Accept", ACCEPT)
                .query(&[("limit", PAGE_SIZE.to_string()), ("page", page.to_string())])
                .send()
                .await?;

            match resp.status() {
                s if s.is_success() => {}
                StatusCode::UNAUTHORIZED => return Err(ProviderError::Unauthorized(AUTH_HINT)),
                s => {
                    return Err(http_error("Gitea", Some(self.base_url.clone()), s));
                }
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

    /// Latest release per repo, for the N most-recently-updated repos. Same
    /// shape and caps as the github / gitlab equivalents — one call per
    /// repo, failures tolerated per-repo.
    pub async fn list_releases(&self) -> Result<Vec<Release>> {
        const MAX_REPOS_TO_CHECK: usize = 60;

        let mut repos = self.list_repos().await?;
        repos.truncate(MAX_REPOS_TO_CHECK);

        let mut handles = Vec::with_capacity(repos.len());
        for repo in repos {
            let client = self.client.clone();
            let token = self.token.clone();
            let base = self.base_url.clone();
            handles.push(tokio::spawn(async move {
                fetch_latest_release(&client, &token, &base, &repo).await
            }));
        }

        let now = Utc::now();
        let mut releases = Vec::new();
        for h in handles {
            if let Ok(Ok(Some(mut r))) = h.await {
                r.is_new = within_days(&r.published_at, &now, 7);
                r.age_human = humanise_age(&r.published_at, now);
                releases.push(r);
            }
        }

        releases.sort_by(|a, b| b.published_at.cmp(&a.published_at));
        Ok(releases)
    }

    /// Latest Gitea Actions workflow run on each repo's default branch.
    /// Best-effort — repos without Actions enabled return 404 and we
    /// surface a "no ci" marker so the row still gets a coloured dot.
    pub async fn list_ci(&self) -> Result<Vec<CiRun>> {
        const MAX_REPOS_TO_CHECK: usize = 60;

        let mut repos = self.list_repos().await?;
        repos.truncate(MAX_REPOS_TO_CHECK);

        let mut handles = Vec::with_capacity(repos.len());
        for repo in repos {
            let client = self.client.clone();
            let token = self.token.clone();
            let base = self.base_url.clone();
            handles.push(tokio::spawn(async move {
                fetch_latest_ci_run(&client, &token, &base, &repo).await
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

#[async_trait::async_trait]
impl ProviderBackend for CodebergProvider {
    fn viewer(&self) -> &Viewer {
        &self.viewer
    }
    fn token(&self) -> &str {
        &self.token
    }
    fn base_url(&self) -> Option<&str> {
        Some(&self.base_url)
    }
    async fn list_waiting(&self) -> Result<Vec<WaitingItem>> {
        self.list_waiting().await
    }
    async fn list_repos(&self) -> Result<Vec<Repo>> {
        self.list_repos().await
    }
    async fn list_releases(&self) -> Result<Vec<Release>> {
        self.list_releases().await
    }
    async fn list_ci(&self) -> Result<Vec<CiRun>> {
        self.list_ci().await
    }
}

fn normalise_base_url(raw: &str) -> Result<String> {
    let trimmed = raw.trim().trim_end_matches('/').to_string();
    if trimmed.is_empty() {
        return Err(ProviderError::InvalidBaseUrl(
            "base URL must not be empty".into(),
        ));
    }
    // HTTPS-only: a self-hosted Gitea/Forgejo base URL is the channel the PAT
    // travels on for every API call. `http://` here would send the bearer
    // token in clear. If a localhost dev-instance ever needs `http://`, gate
    // it explicitly on `localhost` / `127.0.0.1` / `::1` then.
    if !trimmed.starts_with("https://") {
        return Err(ProviderError::InvalidBaseUrl(format!(
            "base URL must start with https://: {trimmed}"
        )));
    }
    Ok(trimmed)
}

async fn fetch_viewer(client: &Client, token: &str, base_url: &str) -> Result<Viewer> {
    let resp = client
        .get(format!("{base_url}/api/v1/user"))
        .bearer_auth(token)
        .header("Accept", ACCEPT)
        .send()
        .await?;

    match resp.status() {
        s if s.is_success() => {
            #[derive(Deserialize)]
            struct Raw {
                login: String,
                full_name: Option<String>,
                avatar_url: Option<String>,
            }
            let r: Raw = resp.json().await?;
            Ok(Viewer {
                login: r.login,
                avatar_url: r.avatar_url,
                name: r.full_name.filter(|s| !s.is_empty()),
            })
        }
        StatusCode::UNAUTHORIZED => Err(ProviderError::Unauthorized(AUTH_HINT)),
        s => Err(http_error("Gitea", Some(base_url.to_string()), s)),
    }
}

async fn search_issues(
    client: &Client,
    token: &str,
    base_url: &str,
    flag: &str,
    reason: ItemReason,
) -> Result<Vec<WaitingItem>> {
    // Gitea's flag-style filters: `type=issues` and one of assigned/created/
    // mentioned/review_requested set to true. We run a second pass for
    // pull requests right after, fanning them through one query each.
    let mut out = Vec::new();
    for kind in ["issues", "pulls"] {
        let resp = client
            .get(format!("{base_url}/api/v1/repos/issues/search"))
            .bearer_auth(token)
            .header("Accept", ACCEPT)
            .query(&[
                ("type", kind),
                ("state", "open"),
                (flag, "true"),
                ("limit", "50"),
            ])
            .send()
            .await?;

        match resp.status() {
            s if s.is_success() => {}
            StatusCode::UNAUTHORIZED => return Err(ProviderError::Unauthorized(AUTH_HINT)),
            s => {
                return Err(http_error("Gitea", Some(base_url.to_string()), s));
            }
        }

        let raw: Vec<RawIssue> = resp.json().await?;
        let now = Utc::now();
        let item_kind = if kind == "pulls" {
            ItemKind::Pr
        } else {
            ItemKind::Is
        };

        for it in raw {
            out.push(WaitingItem {
                id: format!("cb:{}", it.id),
                kind: item_kind,
                title: it.title,
                repo: it
                    .repository
                    .as_ref()
                    .map(|r| r.full_name.clone())
                    .unwrap_or_else(|| extract_repo_from_url(&it.html_url)),
                provider: Provider::Codeberg,
                reason,
                url: it.html_url,
                age_human: humanise_age(&it.updated_at, now),
                updated_at: it.updated_at,
                account_id: None,
            });
        }
    }
    Ok(out)
}

#[derive(Deserialize)]
struct RawIssue {
    id: u64,
    title: String,
    html_url: String,
    updated_at: String,
    repository: Option<RawIssueRepo>,
}

#[derive(Deserialize)]
struct RawIssueRepo {
    full_name: String,
}

/// Fallback when an issue payload doesn't carry a `repository` block:
/// derive `owner/name` from the html_url. Gitea URLs look like
/// `https://codeberg.org/owner/name/issues/42`.
fn extract_repo_from_url(url: &str) -> String {
    let Some((_, rest)) = url.split_once("://") else {
        return String::new();
    };
    let Some((_, path)) = rest.split_once('/') else {
        return String::new();
    };
    // Take the first two path segments.
    let parts: Vec<&str> = path.split('/').take(2).collect();
    if parts.len() == 2 {
        format!("{}/{}", parts[0], parts[1])
    } else {
        String::new()
    }
}

#[derive(Deserialize)]
struct RawRepo {
    id: u64,
    name: String,
    full_name: String,
    default_branch: Option<String>,
    description: Option<String>,
    stars_count: u64,
    html_url: String,
    ssh_url: Option<String>,
    clone_url: Option<String>,
    fork: bool,
    private: bool,
    updated_at: Option<String>,
    /// Time of the last actual push. `updated_at` also moves on metadata
    /// edits, so it's only the fallback for old instances lacking this field.
    pushed_at: Option<String>,
}

impl From<RawRepo> for Repo {
    fn from(r: RawRepo) -> Self {
        let (owner, name) = r
            .full_name
            .rsplit_once('/')
            .map(|(o, n)| (o.to_string(), n.to_string()))
            .unwrap_or_else(|| (String::new(), r.name.clone()));
        Self {
            id: format!("cb:{}", r.id),
            owner,
            name,
            provider: Provider::Codeberg,
            default_branch: r.default_branch.unwrap_or_else(|| "main".into()),
            language: None,
            description: r.description,
            stars: r.stars_count,
            html_url: r.html_url,
            ssh_url: r.ssh_url,
            clone_url: r.clone_url,
            is_fork: r.fork,
            is_private: r.private,
            pushed_at: r.pushed_at.or(r.updated_at),
            account_id: None,
        }
    }
}

async fn fetch_latest_release(
    client: &Client,
    token: &str,
    base_url: &str,
    repo: &Repo,
) -> Result<Option<Release>> {
    let url = format!(
        "{base_url}/api/v1/repos/{}/{}/releases",
        repo.owner, repo.name
    );
    let resp = client
        .get(&url)
        .bearer_auth(token)
        .header("Accept", ACCEPT)
        .query(&[("limit", "1")])
        .send()
        .await?;

    match resp.status() {
        s if s.is_success() => {}
        // 404 = no releases yet, not an error.
        StatusCode::NOT_FOUND => return Ok(None),
        StatusCode::UNAUTHORIZED => return Err(ProviderError::Unauthorized(AUTH_HINT)),
        StatusCode::FORBIDDEN => return Ok(None),
        s => {
            return Err(http_error("Gitea", Some(base_url.to_string()), s));
        }
    }

    #[derive(Deserialize)]
    struct RawRelease {
        tag_name: String,
        name: Option<String>,
        html_url: String,
        published_at: Option<String>,
        #[serde(default)]
        prerelease: bool,
        #[serde(default)]
        draft: bool,
    }

    let raw: Vec<RawRelease> = resp.json().await?;
    // Drafts shouldn't surface in the Releases tab — only published.
    let Some(r) = raw.into_iter().find(|r| !r.draft) else {
        return Ok(None);
    };
    let Some(published_at) = r.published_at else {
        return Ok(None);
    };

    let name = r
        .name
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| r.tag_name.clone());

    Ok(Some(Release {
        repo_id: repo.id.clone(),
        repo_full_name: format!("{}/{}", repo.owner, repo.name),
        provider: Provider::Codeberg,
        tag: r.tag_name,
        name,
        published_at,
        html_url: r.html_url,
        is_prerelease: r.prerelease,
        is_new: false, // filled in by list_releases against a consistent `now`
        age_human: String::new(),
        account_id: None,
    }))
}

/// Gitea wraps the runs in a `workflow_runs` envelope, mirroring GitHub.
/// Hoisted to module level so the deserializer can be unit-tested against
/// recorded fixtures from Gitea 1.21+, older Gitea, and Forgejo (each of
/// which surfaces the actor under a different key).
#[derive(Deserialize)]
struct WorkflowRunsResp {
    workflow_runs: Vec<RawRun>,
}

#[derive(Deserialize)]
struct RawRun {
    status: String,
    #[serde(default)]
    conclusion: Option<String>,
    html_url: String,
    #[serde(default)]
    head_branch: Option<String>,
    #[serde(default)]
    name: Option<String>,
    /// User who triggered the run. Different Gitea/Forgejo versions
    /// surface this under different keys (`actor`, `triggered_by`,
    /// `actor_user`) and on some self-hosted Forgejo instances the
    /// field is absent entirely. Accept the common variants; treat any
    /// None as "we don't know who triggered this" → CI-failure
    /// notifications are silently skipped for that repo
    /// (DECISIONS.md 2026-05-26).
    #[serde(default, alias = "triggered_by", alias = "actor_user")]
    actor: Option<RunActor>,
}

#[derive(Deserialize)]
struct RunActor {
    /// Gitea/Forgejo expose the actor under varying key names too
    /// (`login` on Gitea ≥1.21, `username` on older builds / Forgejo).
    /// Accept both.
    #[serde(alias = "username")]
    login: String,
}

async fn fetch_latest_ci_run(
    client: &Client,
    token: &str,
    base_url: &str,
    repo: &Repo,
) -> Result<Option<CiRun>> {
    let url = format!(
        "{base_url}/api/v1/repos/{}/{}/actions/runs",
        repo.owner, repo.name
    );
    let resp = client
        .get(&url)
        .bearer_auth(token)
        .header("Accept", ACCEPT)
        .query(&[("branch", repo.default_branch.as_str()), ("limit", "1")])
        .send()
        .await?;

    match resp.status() {
        s if s.is_success() => {}
        // Gitea Actions not enabled on this instance, or the repo doesn't
        // have it on. Surface a "no ci" marker so the row still shows a
        // dot (consistent with github/gitlab behaviour).
        StatusCode::NOT_FOUND => {
            return Ok(Some(CiRun {
                repo_id: repo.id.clone(),
                repo_full_name: format!("{}/{}", repo.owner, repo.name),
                status: CiStatus::None,
                html_url: None,
                branch: Some(repo.default_branch.clone()),
                workflow_name: None,
                author_login: None,
                account_id: None,
            }));
        }
        StatusCode::UNAUTHORIZED => return Err(ProviderError::Unauthorized(AUTH_HINT)),
        StatusCode::FORBIDDEN => return Ok(None),
        s => {
            return Err(http_error("Gitea", Some(base_url.to_string()), s));
        }
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
            author_login: None,
            account_id: None,
        }));
    };

    Ok(Some(CiRun {
        repo_id: repo.id.clone(),
        repo_full_name: format!("{}/{}", repo.owner, repo.name),
        status: collapse_ci_status(&run.status, run.conclusion.as_deref()),
        html_url: Some(run.html_url),
        branch: run.head_branch,
        workflow_name: run.name,
        author_login: run.actor.map(|a| a.login),
        account_id: None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_repo_from_codeberg_issue_url() {
        assert_eq!(
            extract_repo_from_url("https://codeberg.org/forgejo/runner/issues/42"),
            "forgejo/runner"
        );
    }

    #[test]
    fn normalises_trailing_slash() {
        assert_eq!(
            normalise_base_url("https://codeberg.org/").unwrap(),
            "https://codeberg.org"
        );
    }

    #[test]
    fn rejects_http_scheme() {
        assert!(normalise_base_url("http://codeberg.example.com").is_err());
    }

    #[test]
    fn rejects_missing_scheme() {
        assert!(normalise_base_url("codeberg.example.com").is_err());
    }

    #[test]
    fn repo_pushed_at_prefers_pushed_at_over_updated_at() {
        // Gitea's `updated_at` moves on metadata edits (description,
        // settings); only `pushed_at` tracks actual pushes. The "recently
        // pushed" sort must use the latter or a description edit bumps the
        // repo to the top.
        let raw: RawRepo = serde_json::from_str(
            r#"{
                "id": 7, "name": "r", "full_name": "o/r",
                "default_branch": "main", "description": null,
                "stars_count": 0, "html_url": "https://codeberg.org/o/r",
                "ssh_url": null, "clone_url": null,
                "fork": false, "private": false,
                "updated_at": "2026-06-01T00:00:00Z",
                "pushed_at": "2026-01-01T00:00:00Z"
            }"#,
        )
        .expect("parse");
        let repo: Repo = raw.into();
        assert_eq!(repo.pushed_at.as_deref(), Some("2026-01-01T00:00:00Z"));
    }

    #[test]
    fn repo_pushed_at_falls_back_to_updated_at() {
        // Very old Gitea builds don't expose `pushed_at` — fall back to
        // `updated_at` rather than dropping the timestamp entirely.
        let raw: RawRepo = serde_json::from_str(
            r#"{
                "id": 7, "name": "r", "full_name": "o/r",
                "default_branch": null, "description": null,
                "stars_count": 0, "html_url": "u", "ssh_url": null,
                "clone_url": null, "fork": false, "private": false,
                "updated_at": "2026-06-01T00:00:00Z"
            }"#,
        )
        .expect("parse");
        let repo: Repo = raw.into();
        assert_eq!(repo.pushed_at.as_deref(), Some("2026-06-01T00:00:00Z"));
    }

    fn fixture(actor_block: &str) -> String {
        format!(
            r#"{{"workflow_runs":[{{
                "status":"completed",
                "conclusion":"failure",
                "html_url":"https://codeberg.org/o/r/actions/runs/1",
                "head_branch":"main",
                "name":"CI"{actor_block}
            }}]}}"#
        )
    }

    #[test]
    fn run_actor_login_modern_gitea() {
        // Gitea ≥1.21 exposes `actor` with `login`, matching GitHub.
        let raw = fixture(r#","actor":{"login":"bjoernw"}"#);
        let resp: WorkflowRunsResp = serde_json::from_str(&raw).expect("parse");
        let run = resp.workflow_runs.into_iter().next().unwrap();
        assert_eq!(run.actor.map(|a| a.login).as_deref(), Some("bjoernw"));
    }

    #[test]
    fn run_actor_login_via_triggered_by_alias() {
        // Older Gitea builds and some Forgejo versions ship the actor as
        // `triggered_by`. The serde alias on `RawRun.actor` accepts it.
        let raw = fixture(r#","triggered_by":{"username":"bjoernw"}"#);
        let resp: WorkflowRunsResp = serde_json::from_str(&raw).expect("parse");
        let run = resp.workflow_runs.into_iter().next().unwrap();
        assert_eq!(run.actor.map(|a| a.login).as_deref(), Some("bjoernw"));
    }

    #[test]
    fn run_actor_login_via_actor_user_alias() {
        // Some Forgejo builds expose the actor as `actor_user`.
        let raw = fixture(r#","actor_user":{"username":"bjoernw"}"#);
        let resp: WorkflowRunsResp = serde_json::from_str(&raw).expect("parse");
        let run = resp.workflow_runs.into_iter().next().unwrap();
        assert_eq!(run.actor.map(|a| a.login).as_deref(), Some("bjoernw"));
    }

    #[test]
    fn run_actor_login_absent_graceful() {
        // A self-hosted Forgejo that doesn't surface an actor at all
        // must not break deserialisation — the CI-failure path silently
        // skips runs with `author_login: None`.
        let raw = fixture("");
        let resp: WorkflowRunsResp = serde_json::from_str(&raw).expect("parse");
        let run = resp.workflow_runs.into_iter().next().unwrap();
        assert!(run.actor.is_none());
    }
}
