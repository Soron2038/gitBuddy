//! GitLab provider — PAT-based auth, waiting items (issues + MRs across the
//! assigned/review-requested/authored scopes), and the project list.
//!
//! Works for both gitlab.com and self-hosted instances (e.g. gitlab.gwdg.de)
//! by taking the base URL at construction time. The token is sent via
//! `Authorization: Bearer …`, which works for both classic PATs and the
//! newer project/group access tokens.

use crate::types::{
    CiRun, CiStatus, ItemKind, ItemReason, Provider, Release, Repo, Viewer, WaitingItem,
};
use chrono::{DateTime, Utc};
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use thiserror::Error;

const USER_AGENT: &str = concat!("gitBuddy/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Error)]
pub enum GitLabError {
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("authentication failed — check that the token is valid and has the `api` scope")]
    Unauthorized,
    #[error("GitLab API at {base_url} returned HTTP {status}")]
    HttpStatus {
        base_url: String,
        status: StatusCode,
    },
    #[error("invalid base URL: {0}")]
    InvalidBaseUrl(String),
}

pub type Result<T> = std::result::Result<T, GitLabError>;

pub struct GitLabProvider {
    client: Client,
    token: String,
    /// Normalised (no trailing slash) base URL, e.g. "https://gitlab.gwdg.de".
    base_url: String,
    pub viewer: Viewer,
}

impl GitLabProvider {
    pub async fn connect(token: String, base_url: String) -> Result<Self> {
        let base_url = normalise_base_url(&base_url)?;
        let client = Client::builder().user_agent(USER_AGENT).build()?;
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

    /// Items where the user is assigned, review-requested, or authored —
    /// across issues and merge requests. GitLab's REST API doesn't expose a
    /// "mentioned" scope, so that filter from GitHub doesn't carry over here.
    pub async fn list_waiting(&self) -> Result<Vec<WaitingItem>> {
        // 5 concurrent fetches: 3 issue scopes + 2 MR scopes (assignee, reviewer).
        let token = self.token.clone();
        let base = self.base_url.clone();

        let queries = vec![
            // (path, query params, item_kind, reason)
            (
                "/api/v4/issues",
                vec![("scope", "assigned_to_me"), ("state", "opened")],
                ItemKind::Is,
                ItemReason::Assigned,
            ),
            (
                "/api/v4/issues",
                vec![("scope", "created_by_me"), ("state", "opened")],
                ItemKind::Is,
                ItemReason::Authored,
            ),
            (
                "/api/v4/merge_requests",
                vec![("scope", "assigned_to_me"), ("state", "opened")],
                ItemKind::Mr,
                ItemReason::Assigned,
            ),
            (
                "/api/v4/merge_requests",
                vec![("scope", "created_by_me"), ("state", "opened")],
                ItemKind::Mr,
                ItemReason::Authored,
            ),
            // GitLab uses reviewer_username for the "review-requested" filter
            // on MRs. We use it specifically rather than the `scope` shorthand
            // which doesn't cover this case.
            (
                "/api/v4/merge_requests",
                vec![
                    ("reviewer_username", self.viewer.login.as_str()),
                    ("state", "opened"),
                ],
                ItemKind::Mr,
                ItemReason::Review,
            ),
        ];

        let mut handles = Vec::with_capacity(queries.len());
        for (path, params, kind, reason) in queries {
            let client = self.client.clone();
            let token = token.clone();
            let base = base.clone();
            let params: Vec<(String, String)> = params
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            handles.push(tokio::spawn(async move {
                fetch_items(&client, &token, &base, path, &params, kind, reason).await
            }));
        }

        let mut items = Vec::new();
        for h in handles {
            match h.await {
                Ok(Ok(mut v)) => items.append(&mut v),
                Ok(Err(e)) => return Err(e),
                Err(_) => {
                    return Err(GitLabError::HttpStatus {
                        base_url: base.clone(),
                        status: StatusCode::INTERNAL_SERVER_ERROR,
                    });
                }
            }
        }

        // Dedup: a single MR can match assigned + review scopes, etc.
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
        const PAGE_SIZE: u32 = 100;
        const MAX_PAGES: u32 = 5;

        for page in 1..=MAX_PAGES {
            let resp = self
                .client
                .get(format!("{}/api/v4/projects", self.base_url))
                .bearer_auth(&self.token)
                .query(&[
                    ("membership", "true"),
                    ("per_page", &PAGE_SIZE.to_string()),
                    ("page", &page.to_string()),
                    ("order_by", "last_activity_at"),
                ])
                .send()
                .await?;

            match resp.status() {
                s if s.is_success() => {}
                StatusCode::UNAUTHORIZED => return Err(GitLabError::Unauthorized),
                s => {
                    return Err(GitLabError::HttpStatus {
                        base_url: self.base_url.clone(),
                        status: s,
                    });
                }
            }

            let raw: Vec<RawProject> = resp.json().await?;
            let len = raw.len();
            all.extend(raw.into_iter().map(|p| p.into_repo(self.is_self_hosted())));
            if (len as u32) < PAGE_SIZE {
                break;
            }
        }

        Ok(all)
    }

    /// Latest release per project, for the N most-recently-active projects
    /// the viewer has access to. Bounded for the same reason as the GitHub
    /// equivalent — one HTTP call per project, and dormant archives don't
    /// merit the spend.
    pub async fn list_releases(&self) -> Result<Vec<Release>> {
        const MAX_PROJECTS_TO_CHECK: usize = 60;

        let mut repos = self.list_repos().await?;
        repos.truncate(MAX_PROJECTS_TO_CHECK);
        let self_hosted = self.is_self_hosted();

        let mut handles = Vec::with_capacity(repos.len());
        for repo in repos {
            let client = self.client.clone();
            let token = self.token.clone();
            let base = self.base_url.clone();
            handles.push(tokio::spawn(async move {
                fetch_latest_release(&client, &token, &base, &repo, self_hosted).await
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

    /// Latest pipeline run on each project's default branch. Used by the UI's
    /// per-repo CI status dot. GitLab's pipeline list endpoint is filtered
    /// by `ref` (branch) and `per_page=1` to get just the head.
    pub async fn list_ci(&self) -> Result<Vec<CiRun>> {
        const MAX_PROJECTS_TO_CHECK: usize = 60;

        let mut repos = self.list_repos().await?;
        repos.truncate(MAX_PROJECTS_TO_CHECK);

        let mut handles = Vec::with_capacity(repos.len());
        for repo in repos {
            let client = self.client.clone();
            let token = self.token.clone();
            let base = self.base_url.clone();
            handles.push(tokio::spawn(async move {
                fetch_latest_pipeline(&client, &token, &base, &repo).await
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

    /// Heuristic — anything other than gitlab.com is treated as a "self-
    /// hosted GitLab" for tagging purposes in the UI. This is purely cosmetic
    /// at the moment; the per-instance distinction is in `provider` tag.
    fn is_self_hosted(&self) -> bool {
        !self.base_url.contains("gitlab.com")
    }
}

fn normalise_base_url(raw: &str) -> Result<String> {
    let trimmed = raw.trim().trim_end_matches('/').to_string();
    if trimmed.is_empty() {
        return Err(GitLabError::InvalidBaseUrl(
            "base URL must not be empty".into(),
        ));
    }
    if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
        return Err(GitLabError::InvalidBaseUrl(format!(
            "base URL must start with http:// or https://: {trimmed}"
        )));
    }
    Ok(trimmed)
}

async fn fetch_viewer(client: &Client, token: &str, base_url: &str) -> Result<Viewer> {
    let resp = client
        .get(format!("{base_url}/api/v4/user"))
        .bearer_auth(token)
        .send()
        .await?;

    match resp.status() {
        s if s.is_success() => {
            #[derive(Deserialize)]
            struct Raw {
                username: String,
                name: Option<String>,
                avatar_url: Option<String>,
            }
            let r: Raw = resp.json().await?;
            Ok(Viewer {
                login: r.username,
                avatar_url: r.avatar_url,
                name: r.name,
            })
        }
        StatusCode::UNAUTHORIZED => Err(GitLabError::Unauthorized),
        s => Err(GitLabError::HttpStatus {
            base_url: base_url.to_string(),
            status: s,
        }),
    }
}

async fn fetch_items(
    client: &Client,
    token: &str,
    base_url: &str,
    path: &str,
    params: &[(String, String)],
    kind: ItemKind,
    reason: ItemReason,
) -> Result<Vec<WaitingItem>> {
    let mut params: Vec<(&str, &str)> = params
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    params.push(("per_page", "50"));

    let resp = client
        .get(format!("{base_url}{path}"))
        .bearer_auth(token)
        .query(&params)
        .send()
        .await?;

    match resp.status() {
        s if s.is_success() => {}
        StatusCode::UNAUTHORIZED => return Err(GitLabError::Unauthorized),
        s => {
            return Err(GitLabError::HttpStatus {
                base_url: base_url.to_string(),
                status: s,
            });
        }
    }

    let raw: Vec<RawItem> = resp.json().await?;
    let now = Utc::now();
    let provider = pick_provider(base_url);

    Ok(raw
        .into_iter()
        .map(|it| WaitingItem {
            id: it.id.to_string(),
            kind,
            title: it.title,
            repo: it
                .references
                .as_ref()
                .and_then(|r| r.full.as_deref())
                .map(strip_iid)
                .unwrap_or_else(|| extract_path_from_url(&it.web_url)),
            provider,
            reason,
            url: it.web_url,
            age_human: humanise_age(&it.updated_at, now),
            updated_at: it.updated_at,
            account_id: None,
        })
        .collect())
}

#[derive(Deserialize)]
struct RawItem {
    id: u64,
    title: String,
    web_url: String,
    updated_at: String,
    references: Option<RawRefs>,
}

#[derive(Deserialize)]
struct RawRefs {
    /// e.g. "group/project#42" — we strip the "#42" to get a repo full-name.
    full: Option<String>,
}

fn strip_iid(reference: &str) -> String {
    reference
        .split(['#', '!'])
        .next()
        .unwrap_or(reference)
        .to_string()
}

/// Fallback when references.full is missing: derive "group/project" from
/// the issue/MR's web_url. URL looks like:
/// `https://gitlab.example.com/group/sub/project/-/issues/42`.
fn extract_path_from_url(url: &str) -> String {
    // strip scheme
    let Some((_, rest)) = url.split_once("://") else {
        return String::new();
    };
    // strip host (everything before the first slash)
    let Some((_, path)) = rest.split_once('/') else {
        return String::new();
    };
    path.split("/-/").next().unwrap_or(path).to_string()
}

#[derive(Deserialize)]
struct RawProject {
    id: u64,
    path_with_namespace: String,
    default_branch: Option<String>,
    description: Option<String>,
    star_count: u64,
    web_url: String,
    ssh_url_to_repo: Option<String>,
    http_url_to_repo: Option<String>,
    forked_from_project: Option<serde_json::Value>,
    visibility: String,
    last_activity_at: Option<String>,
}

impl RawProject {
    fn into_repo(self, self_hosted: bool) -> Repo {
        // `path_with_namespace` is the URL-form: "group/sub/repo-slug". We
        // split off the last segment for `name` and use the rest as `owner`.
        // We deliberately ignore `self.name` (the human display name), which
        // can contain spaces ("Netbox Backup") that wouldn't match the local
        // clone's parsed origin URL ("Netbox-Backup") — and the local-index
        // join would silently fail.
        let (owner, name) = match self.path_with_namespace.rsplit_once('/') {
            Some((o, n)) => (o.to_string(), n.to_string()),
            None => (String::new(), self.path_with_namespace.clone()),
        };
        Repo {
            id: format!("gl:{}", self.id),
            owner,
            name,
            provider: if self_hosted {
                Provider::MpsdGitlab
            } else {
                Provider::Gitlab
            },
            default_branch: self.default_branch.unwrap_or_else(|| "main".into()),
            language: None, // GitLab doesn't expose a single primary language on /projects
            description: self.description,
            stars: self.star_count,
            html_url: self.web_url,
            ssh_url: self.ssh_url_to_repo,
            clone_url: self.http_url_to_repo,
            is_fork: self.forked_from_project.is_some(),
            is_private: self.visibility != "public",
            pushed_at: self.last_activity_at,
            account_id: None,
        }
    }
}

fn pick_provider(base_url: &str) -> Provider {
    if base_url.contains("gitlab.com") {
        Provider::Gitlab
    } else {
        // Tag self-hosted GitLabs distinctly so the UI can label them with
        // the instance name rather than the generic "GitLab" pill.
        Provider::MpsdGitlab
    }
}

fn humanise_age(ts: &str, now: DateTime<Utc>) -> String {
    let Ok(t) = DateTime::parse_from_rfc3339(ts) else {
        return "?".into();
    };
    let mins = (now - t.with_timezone(&Utc)).num_minutes();
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

fn within_days(timestamp: &str, now: &DateTime<Utc>, days: i64) -> bool {
    DateTime::parse_from_rfc3339(timestamp)
        .map(|t| (*now - t.with_timezone(&Utc)).num_days() <= days)
        .unwrap_or(false)
}

/// Repo.id from `into_repo` is shaped `"gl:<numeric>"` so the local-index
/// join can tell GitLab and GitHub ids apart. The release/pipeline
/// endpoints want the raw numeric id back; this peels the prefix.
fn project_id_from_repo(repo: &Repo) -> Option<&str> {
    repo.id.strip_prefix("gl:")
}

async fn fetch_latest_release(
    client: &Client,
    token: &str,
    base_url: &str,
    repo: &Repo,
    self_hosted: bool,
) -> Result<Option<Release>> {
    let Some(project_id) = project_id_from_repo(repo) else {
        return Ok(None);
    };
    let url = format!("{base_url}/api/v4/projects/{project_id}/releases");
    let resp = client
        .get(&url)
        .bearer_auth(token)
        .query(&[("per_page", "1")])
        .send()
        .await?;

    match resp.status() {
        s if s.is_success() => {}
        // 404 means the project has no releases yet (or the project itself
        // is gone) — not an error.
        StatusCode::NOT_FOUND => return Ok(None),
        StatusCode::UNAUTHORIZED => return Err(GitLabError::Unauthorized),
        // 403 happens on archived projects under some visibility settings —
        // graceful no-op rather than failing the whole batch.
        StatusCode::FORBIDDEN => return Ok(None),
        s => {
            return Err(GitLabError::HttpStatus {
                base_url: base_url.to_string(),
                status: s,
            });
        }
    }

    #[derive(Deserialize)]
    struct RawRelease {
        tag_name: String,
        name: Option<String>,
        released_at: Option<String>,
        #[serde(default)]
        upcoming_release: bool,
        #[serde(default)]
        #[serde(rename = "_links")]
        links: Option<RawLinks>,
    }
    #[derive(Deserialize)]
    struct RawLinks {
        #[serde(rename = "self")]
        self_url: Option<String>,
    }

    let raw: Vec<RawRelease> = resp.json().await?;
    let Some(r) = raw.into_iter().next() else {
        return Ok(None);
    };
    let Some(released_at) = r.released_at else {
        // GitLab can list a release without `released_at` (drafts /
        // upcoming) — skip those, the UI only shows published ones.
        return Ok(None);
    };

    let name = r
        .name
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| r.tag_name.clone());
    // Prefer the canonical release page URL when GitLab provides it; fall
    // back to a constructed one keyed off the project's web_url so the
    // "Open release" button always lands somewhere sensible.
    let html_url = r
        .links
        .and_then(|l| l.self_url)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("{}/-/releases/{}", repo.html_url, r.tag_name));

    Ok(Some(Release {
        repo_id: repo.id.clone(),
        repo_full_name: format!("{}/{}", repo.owner, repo.name),
        provider: if self_hosted {
            Provider::MpsdGitlab
        } else {
            Provider::Gitlab
        },
        tag: r.tag_name,
        name,
        published_at: released_at,
        html_url,
        is_prerelease: r.upcoming_release,
        is_new: false, // filled in by list_releases against a consistent `now`
        age_human: String::new(),
        account_id: None,
    }))
}

async fn fetch_latest_pipeline(
    client: &Client,
    token: &str,
    base_url: &str,
    repo: &Repo,
) -> Result<Option<CiRun>> {
    let Some(project_id) = project_id_from_repo(repo) else {
        return Ok(None);
    };
    let url = format!("{base_url}/api/v4/projects/{project_id}/pipelines");
    let resp = client
        .get(&url)
        .bearer_auth(token)
        .query(&[("ref", repo.default_branch.as_str()), ("per_page", "1")])
        .send()
        .await?;

    match resp.status() {
        s if s.is_success() => {}
        // Project has no pipelines / CI not configured — emit a None marker
        // so the repo row still gets a "no ci" dot rather than vanishing.
        StatusCode::NOT_FOUND => {
            return Ok(Some(CiRun {
                repo_id: repo.id.clone(),
                repo_full_name: format!("{}/{}", repo.owner, repo.name),
                status: CiStatus::None,
                html_url: None,
                branch: Some(repo.default_branch.clone()),
                workflow_name: None,
                account_id: None,
            }));
        }
        StatusCode::UNAUTHORIZED => return Err(GitLabError::Unauthorized),
        StatusCode::FORBIDDEN => return Ok(None),
        s => {
            return Err(GitLabError::HttpStatus {
                base_url: base_url.to_string(),
                status: s,
            });
        }
    }

    #[derive(Deserialize)]
    struct RawPipeline {
        status: String,
        web_url: String,
        #[serde(rename = "ref")]
        ref_: Option<String>,
        #[serde(default)]
        name: Option<String>,
    }

    let raw: Vec<RawPipeline> = resp.json().await?;
    let Some(p) = raw.into_iter().next() else {
        return Ok(Some(CiRun {
            repo_id: repo.id.clone(),
            repo_full_name: format!("{}/{}", repo.owner, repo.name),
            status: CiStatus::None,
            html_url: None,
            branch: Some(repo.default_branch.clone()),
            workflow_name: None,
            account_id: None,
        }));
    };

    Ok(Some(CiRun {
        repo_id: repo.id.clone(),
        repo_full_name: format!("{}/{}", repo.owner, repo.name),
        status: collapse_pipeline_status(&p.status),
        html_url: Some(p.web_url),
        branch: p.ref_,
        workflow_name: p.name,
        account_id: None,
    }))
}

/// Map GitLab's pipeline status onto the four buckets the UI cares about.
/// GitLab's full set: created, waiting_for_resource, preparing, pending,
/// running, success, failed, canceled, skipped, manual, scheduled.
/// "manual" pipelines wait for a human to click "play" — surfacing them as
/// "no ci" rather than "running" matches the actual user-visible state.
fn collapse_pipeline_status(status: &str) -> CiStatus {
    match status {
        "success" => CiStatus::Ok,
        "failed" => CiStatus::Fail,
        "running" | "pending" | "preparing" | "created" | "waiting_for_resource" | "scheduled" => {
            CiStatus::Run
        }
        "canceled" | "skipped" => CiStatus::Cancelled,
        // "manual" or anything we don't recognise yet
        _ => CiStatus::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalises_trailing_slash() {
        assert_eq!(
            normalise_base_url("https://gitlab.gwdg.de/").unwrap(),
            "https://gitlab.gwdg.de"
        );
    }

    #[test]
    fn rejects_missing_scheme() {
        assert!(normalise_base_url("gitlab.example.com").is_err());
    }

    #[test]
    fn strip_iid_handles_issue_and_mr_refs() {
        assert_eq!(strip_iid("group/sub/project#42"), "group/sub/project");
        assert_eq!(strip_iid("group/project!17"), "group/project");
        assert_eq!(strip_iid("plain/path"), "plain/path");
    }

    #[test]
    fn extract_path_from_url_works() {
        assert_eq!(
            extract_path_from_url("https://gitlab.gwdg.de/group/sub/repo/-/issues/42"),
            "group/sub/repo"
        );
    }
}
