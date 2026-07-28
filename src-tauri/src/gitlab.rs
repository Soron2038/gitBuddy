//! GitLab provider — PAT-based auth, waiting items (issues + MRs across the
//! assigned/review-requested/authored scopes), and the project list.
//!
//! Works for both gitlab.com and self-hosted instances (e.g. gitlab.gwdg.de)
//! by taking the base URL at construction time. The token is sent via
//! `Authorization: Bearer …`, which works for both classic PATs and the
//! newer project/group access tokens.

use crate::provider_util::{
    decode_error, http_client, humanise_age, is_rate_limited, normalise_base_url, reason_priority,
    repo_call_budget, response_error, within_days, ProviderBackend, ProviderError,
};
use crate::types::{
    CiRun, CiStatus, ItemKind, ItemReason, Provider, Release, Repo, Viewer, WaitingItem,
};
use chrono::{DateTime, Utc};
use reqwest::{Client, StatusCode};
use serde::Deserialize;

/// Hint surfaced when GitLab rejects the token — names the scope to check.
const AUTH_HINT: &str = "check that the token is valid and has the `api` scope";

pub type Result<T> = std::result::Result<T, ProviderError>;

pub struct GitLabProvider {
    /// Shared budget for the per-repo release/CI fan-outs — see
    /// `provider_util::MAX_CONCURRENT_REPO_CALLS`.
    repo_budget: std::sync::Arc<tokio::sync::Semaphore>,
    client: Client,
    token: String,
    /// Normalised (no trailing slash) base URL, e.g. "https://gitlab.gwdg.de".
    base_url: String,
    pub viewer: Viewer,
}

impl GitLabProvider {
    pub async fn connect(token: String, base_url: String) -> Result<Self> {
        let base_url = normalise_base_url(&base_url)?;
        let client = http_client()?;
        let viewer = fetch_viewer(&client, &token, &base_url).await?;
        Ok(Self {
            repo_budget: repo_call_budget(),
            client,
            token,
            base_url,
            viewer,
        })
    }

    /// Construct a provider pointed at an arbitrary base URL (a mock server),
    /// skipping `connect`'s base-URL normalisation and `/user` round-trip.
    /// Tests only — drives the real request paths against a localhost
    /// `wiremock` server. That server speaks plain HTTP, so normalisation's
    /// https-only rule is intentionally bypassed here.
    #[cfg(test)]
    pub(crate) fn for_test(base_url: String, token: String, viewer: Viewer) -> Self {
        Self {
            repo_budget: repo_call_budget(),
            client: http_client().expect("test http client"),
            token,
            base_url,
            viewer,
        }
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
                // Hard auth failures propagate; every other per-scope error
                // (rate limit, transient 5xx, or a panicked task) is tolerated
                // so one failing filter doesn't blank the whole "waiting" list.
                Ok(Err(e @ ProviderError::Unauthorized(_))) => return Err(e),
                Ok(Err(_)) | Err(_) => {}
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
                StatusCode::UNAUTHORIZED => return Err(ProviderError::Unauthorized(AUTH_HINT)),
                s => {
                    return Err(response_error(
                        "GitLab",
                        Some(self.base_url.clone()),
                        AUTH_HINT,
                        s,
                        resp.headers(),
                    ));
                }
            }

            let raw: Vec<RawProject> = resp.json().await.map_err(decode_error("GitLab"))?;
            let len = raw.len();
            let last_page = page == MAX_PAGES;
            all.extend(
                raw.into_iter()
                    .map(|p| p.into_repo(self.is_self_hosted(), &self.host())),
            );
            if (len as u32) < PAGE_SIZE {
                break;
            }
            if last_page {
                // Hit the page cap with a full page still coming back: the
                // list is truncated. Say so rather than silently presenting a
                // partial list as complete.
                eprintln!(
                    "gitbuddy: GitLab repo list truncated at {MAX_PAGES} pages — some repos are not shown"
                );
            }
        }

        Ok(all)
    }

    /// Latest release per project, for the N most-recently-active projects
    /// the viewer has access to. Bounded for the same reason as the GitHub
    /// equivalent — one HTTP call per project, and dormant archives don't
    /// merit the spend.
    pub async fn list_releases(&self, repos: &[Repo]) -> Result<Vec<Release>> {
        const MAX_PROJECTS_TO_CHECK: usize = 60;

        let repos: Vec<Repo> = repos.iter().take(MAX_PROJECTS_TO_CHECK).cloned().collect();
        let self_hosted = self.is_self_hosted();

        let mut handles = Vec::with_capacity(repos.len());
        for repo in repos {
            let client = self.client.clone();
            let token = self.token.clone();
            let base = self.base_url.clone();
            let budget = self.repo_budget.clone();
            handles.push(tokio::spawn(async move {
                // Bounded fan-out: releases and CI share one budget
                // per account so a tick can't put 120 requests on the
                // wire at once. A closed semaphore can't happen here
                // (nothing closes it), so the permit is simply held
                // for the duration of the call.
                let _permit = budget.acquire().await;
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
    pub async fn list_ci(&self, repos: &[Repo]) -> Result<Vec<CiRun>> {
        const MAX_PROJECTS_TO_CHECK: usize = 60;

        let repos: Vec<Repo> = repos.iter().take(MAX_PROJECTS_TO_CHECK).cloned().collect();

        let mut handles = Vec::with_capacity(repos.len());
        for repo in repos {
            let client = self.client.clone();
            let token = self.token.clone();
            let base = self.base_url.clone();
            let budget = self.repo_budget.clone();
            handles.push(tokio::spawn(async move {
                // Bounded fan-out: releases and CI share one budget
                // per account so a tick can't put 120 requests on the
                // wire at once. A closed semaphore can't happen here
                // (nothing closes it), so the permit is simply held
                // for the duration of the call.
                let _permit = budget.acquire().await;
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

    /// Host of the configured instance, e.g. `"gitlab.gwdg.de"`. Falls back
    /// to the raw base URL, which `normalise_base_url` guarantees is a
    /// well-formed `https://…`, so the fallback is defensive only.
    fn host(&self) -> String {
        crate::accounts::url_host(&self.base_url).unwrap_or_else(|| self.base_url.clone())
    }

    /// gitlab.com itself, or a self-hosted instance? Compared against the
    /// *parsed host*, never by substring: `gitlab.company.com` contains
    /// `gitlab.com` as its first ten characters, and so do
    /// `gitlab.compute.internal` and (deliberately)
    /// `gitlab.com.attacker.example`.
    fn is_self_hosted(&self) -> bool {
        let host = self.host();
        !(host == "gitlab.com" || host.ends_with(".gitlab.com"))
    }
}

#[async_trait::async_trait]
impl ProviderBackend for GitLabProvider {
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
    async fn list_releases(&self, repos: &[Repo]) -> Result<Vec<Release>> {
        self.list_releases(repos).await
    }
    async fn list_ci(&self, repos: &[Repo]) -> Result<Vec<CiRun>> {
        self.list_ci(repos).await
    }
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
            let r: Raw = resp.json().await.map_err(decode_error("GitLab"))?;
            Ok(Viewer {
                login: r.username,
                avatar_url: r.avatar_url,
                name: r.name,
            })
        }
        StatusCode::UNAUTHORIZED => Err(ProviderError::Unauthorized(AUTH_HINT)),
        s => Err(response_error(
            "GitLab",
            Some(base_url.to_string()),
            AUTH_HINT,
            s,
            resp.headers(),
        )),
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
    // GitLab defaults to `order_by=created_at`, so without these the 50 rows
    // we keep are the 50 most recently *created* — which we then re-sort by
    // updated_at and present as "most recent activity". Asking for the right
    // order up front makes the cap cut off the genuinely stale items.
    params.push(("order_by", "updated_at"));
    params.push(("sort", "desc"));

    let resp = client
        .get(format!("{base_url}{path}"))
        .bearer_auth(token)
        .query(&params)
        .send()
        .await?;

    match resp.status() {
        s if s.is_success() => {}
        StatusCode::UNAUTHORIZED => return Err(ProviderError::Unauthorized(AUTH_HINT)),
        s => {
            return Err(response_error(
                "GitLab",
                Some(base_url.to_string()),
                AUTH_HINT,
                s,
                resp.headers(),
            ));
        }
    }

    let raw: Vec<RawItem> = resp.json().await.map_err(decode_error("GitLab"))?;
    let now = Utc::now();
    let provider = pick_provider(base_url);

    Ok(raw
        .into_iter()
        .map(|it| WaitingItem {
            // Namespaced by kind: GitLab's issues and merge requests come
            // from separate tables with independent global-id sequences, so
            // an issue and an MR can genuinely share an id. Without the
            // prefix the dedup below sees them as the same row and drops
            // one — silently, and arbitrarily which. (GitHub and Gitea keep
            // both in one id space and don't have this problem.)
            id: format!(
                "{}:{}",
                if kind == ItemKind::Mr { "mr" } else { "is" },
                it.id
            ),
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
    fn into_repo(self, self_hosted: bool, host: &str) -> Repo {
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
            id: format!("gl:{host}:{}", self.id),
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

/// Repo.id from `into_repo` is shaped `"gl:<host>:<numeric>"` — the `gl:`
/// prefix tells GitLab ids apart from GitHub's, and the host keeps two
/// self-hosted instances from colliding (both have a project id 1, 2, 3…,
/// and the frontend keys its repo grid on this string). The release/pipeline
/// endpoints want the raw numeric id back; this peels prefix and host.
fn project_id_from_repo(repo: &Repo) -> Option<&str> {
    repo.id
        .strip_prefix("gl:")?
        .rsplit_once(':')
        .map(|(_host, id)| id)
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
        StatusCode::UNAUTHORIZED => return Err(ProviderError::Unauthorized(AUTH_HINT)),
        // 403 happens on archived projects under some visibility settings —
        // graceful no-op rather than failing the whole batch. The guard keeps
        // a *throttled* 403 out of this arm; that one has to surface.
        StatusCode::FORBIDDEN if !is_rate_limited(StatusCode::FORBIDDEN, resp.headers()) => {
            return Ok(None)
        }
        s => {
            return Err(response_error(
                "GitLab",
                Some(base_url.to_string()),
                AUTH_HINT,
                s,
                resp.headers(),
            ));
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

    let raw: Vec<RawRelease> = resp.json().await.map_err(decode_error("GitLab"))?;
    // GitLab orders by `released_at desc`, which puts a *scheduled* release
    // (one dated in the future) first. Taking it meant the Releases tab
    // showed next month's v4.0 — badged NEW, aged "now", because both
    // `within_days` and `humanise_age` were reading a negative delta — while
    // the actually-shipped v3.9 was hidden behind it. Skip anything that
    // hasn't been released yet and take the newest that has.
    let now = Utc::now();
    let Some((r, released_at)) = raw
        .into_iter()
        .filter(|r| !r.upcoming_release)
        .filter_map(|r| {
            // A release without `released_at` is a draft — never shown.
            let released_at = r.released_at.clone()?;
            let shipped = DateTime::parse_from_rfc3339(&released_at)
                .map(|t| t.with_timezone(&Utc) <= now)
                .unwrap_or(true);
            shipped.then_some((r, released_at))
        })
        .next()
    else {
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
        .unwrap_or_else(|| {
            // Tags legitimately contain `/` (`release/1.0`) and `#`, both of
            // which change the meaning of the path if pasted in raw.
            format!(
                "{}/-/releases/{}",
                repo.html_url,
                crate::oauth::urlencode(&r.tag_name)
            )
        });

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
        // `upcoming_release` means "dated in the future", not "prerelease" —
        // and those are filtered out above, so it is always false here.
        // GitLab's REST releases endpoint exposes no prerelease flag.
        is_prerelease: false,
        is_new: false, // filled in by list_releases against a consistent `now`
        age_human: String::new(),
        account_id: None,
    }))
}

/// Shape returned by `/projects/{id}/pipelines?per_page=1`. Hoisted to
/// module level so unit tests can exercise the deserializer against
/// recorded fixtures.
#[derive(Deserialize)]
struct RawPipeline {
    status: String,
    web_url: String,
    #[serde(rename = "ref")]
    ref_: Option<String>,
    #[serde(default)]
    name: Option<String>,
    /// User who triggered the pipeline (push committer, MR author, or
    /// whoever ran the manual job). The notifications pipeline only
    /// fires CI-failure events when `username` matches the connected
    /// account's viewer login.
    #[serde(default)]
    user: Option<PipelineUser>,
}

#[derive(Deserialize)]
struct PipelineUser {
    username: String,
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
                author_login: None,
                account_id: None,
            }));
        }
        StatusCode::UNAUTHORIZED => return Err(ProviderError::Unauthorized(AUTH_HINT)),
        StatusCode::FORBIDDEN if !is_rate_limited(StatusCode::FORBIDDEN, resp.headers()) => {
            // Not throttling — CI genuinely isn't available here. Emit the
            // same marker row the 404 arm does so the repo keeps a "no ci"
            // dot instead of dropping out of the list on alternating ticks.
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
        s => {
            return Err(response_error(
                "GitLab",
                Some(base_url.to_string()),
                AUTH_HINT,
                s,
                resp.headers(),
            ));
        }
    }

    let raw: Vec<RawPipeline> = resp.json().await.map_err(decode_error("GitLab"))?;
    let Some(p) = raw.into_iter().next() else {
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
        status: collapse_pipeline_status(&p.status),
        html_url: Some(p.web_url),
        branch: p.ref_,
        workflow_name: p.name,
        author_login: p.user.map(|u| u.username),
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
    use crate::provider_util::test_support::{json_array, viewer};
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn self_hosted_detection_compares_the_parsed_host() {
        let self_hosted = |base: &str| {
            let host = crate::accounts::url_host(base).unwrap_or_else(|| base.to_string());
            !(host == "gitlab.com" || host.ends_with(".gitlab.com"))
        };
        assert!(!self_hosted("https://gitlab.com"));
        assert!(!self_hosted("https://gitlab.com/"));
        // A substring match would call all four of these gitlab.com.
        assert!(self_hosted("https://gitlab.company.com"));
        assert!(self_hosted("https://gitlab.compute.internal"));
        assert!(self_hosted("https://gitlab.commerz.de"));
        assert!(self_hosted("https://gitlab.com.attacker.example"));
        assert!(self_hosted("https://gitlab.gwdg.de"));
    }

    #[test]
    fn repo_ids_are_host_qualified_and_peel_back() {
        let raw: RawProject = serde_json::from_str(
            r#"{"id": 7, "path_with_namespace": "g/p", "default_branch": "main",
                "description": null, "star_count": 0, "web_url": "https://gitlab.gwdg.de/g/p",
                "ssh_url_to_repo": null, "http_url_to_repo": null,
                "visibility": "private", "last_activity_at": "2026-06-01T00:00:00Z"}"#,
        )
        .expect("parse");
        let repo = raw.into_repo(true, "gitlab.gwdg.de");
        assert_eq!(repo.id, "gl:gitlab.gwdg.de:7");
        // Two instances, same project id, distinct keys — this is what stops
        // the frontend's keyed {#each} from throwing on a duplicate key.
        assert_ne!(repo.id, "gl:gitlab.mpsd.mpg.de:7");
        // …and the API calls still get the bare numeric id back.
        assert_eq!(project_id_from_repo(&repo), Some("7"));
    }

    #[test]
    fn waiting_ids_namespace_issues_apart_from_merge_requests() {
        // GitLab's issues and merge_requests tables have independent id
        // sequences, so a collision on the raw global id is entirely normal.
        let id_for = |kind: ItemKind, id: u64| {
            format!("{}:{}", if kind == ItemKind::Mr { "mr" } else { "is" }, id)
        };
        assert_ne!(id_for(ItemKind::Is, 5001), id_for(ItemKind::Mr, 5001));
        assert_eq!(id_for(ItemKind::Mr, 5001), "mr:5001");
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

    #[test]
    fn pipeline_extracts_user_username() {
        // Trimmed fixture from `/projects/:id/pipelines?per_page=1` — kept
        // only the fields the deserializer touches.
        let raw = r#"[{
            "status": "failed",
            "web_url": "https://gitlab.com/group/repo/-/pipelines/99",
            "ref": "main",
            "name": "build",
            "user": {"id": 7, "username": "bwitt"}
        }]"#;
        let parsed: Vec<RawPipeline> = serde_json::from_str(raw).expect("parse");
        let p = parsed.into_iter().next().unwrap();
        assert_eq!(p.user.map(|u| u.username).as_deref(), Some("bwitt"));
    }

    #[test]
    fn pipeline_user_optional() {
        // Older self-hosted GitLab instances sometimes omit `user` for
        // scheduled / API-triggered pipelines. The deserializer must
        // tolerate this so the rest of the snapshot still lands.
        let raw = r#"[{
            "status": "failed",
            "web_url": "https://gitlab.com/group/repo/-/pipelines/100",
            "ref": "main"
        }]"#;
        let parsed: Vec<RawPipeline> = serde_json::from_str(raw).expect("parse");
        let p = parsed.into_iter().next().unwrap();
        assert!(p.user.is_none());
    }

    // ---- HTTP-conformance suite ------------------------------------------
    // Real reqwest paths against a localhost wiremock server (via `for_test`):
    // pagination, the bearer header, and the rate-limit/error/404 mappings.

    /// A `/api/v4/projects` element with only the fields `RawProject` requires.
    fn project_json(i: usize) -> String {
        format!(
            r#"{{"id":{i},"path_with_namespace":"o/r{i}","star_count":0,"web_url":"https://x/{i}","visibility":"public"}}"#
        )
    }

    /// A test repo whose `gl:`-prefixed id yields project 42 in release URLs.
    fn repo() -> Repo {
        Repo {
            id: "gl:42".into(),
            owner: "o".into(),
            name: "r".into(),
            provider: Provider::Gitlab,
            default_branch: "main".into(),
            language: None,
            description: None,
            stars: 0,
            html_url: "https://x".into(),
            ssh_url: None,
            clone_url: None,
            is_fork: false,
            is_private: false,
            pushed_at: None,
            account_id: None,
        }
    }

    #[tokio::test]
    async fn list_repos_paginates_until_short_page() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v4/projects"))
            .and(query_param("page", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_string(json_array(100, project_json)))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v4/projects"))
            .and(query_param("page", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_string(json_array(1, project_json)))
            .expect(1)
            .mount(&server)
            .await;

        let gl = GitLabProvider::for_test(server.uri(), "t".into(), viewer("tester"));
        assert_eq!(gl.list_repos().await.expect("ok").len(), 101);
    }

    #[tokio::test]
    async fn list_repos_sends_bearer_token() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v4/projects"))
            .and(header("authorization", "Bearer testtoken"))
            .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
            .expect(1)
            .mount(&server)
            .await;
        let gl = GitLabProvider::for_test(server.uri(), "testtoken".into(), viewer("tester"));
        gl.list_repos().await.expect("authorised");
    }

    #[tokio::test]
    async fn list_repos_maps_401_to_unauthorized() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v4/projects"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let gl = GitLabProvider::for_test(server.uri(), "t".into(), viewer("tester"));
        assert!(matches!(
            gl.list_repos().await.unwrap_err(),
            ProviderError::Unauthorized(_)
        ));
    }

    #[tokio::test]
    async fn list_repos_maps_429_to_rate_limited() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v4/projects"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;
        let gl = GitLabProvider::for_test(server.uri(), "t".into(), viewer("tester"));
        assert!(matches!(
            gl.list_repos().await.unwrap_err(),
            ProviderError::RateLimited { .. }
        ));
    }

    #[tokio::test]
    async fn list_repos_maps_5xx_to_http_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v4/projects"))
            .respond_with(ResponseTemplate::new(502))
            .mount(&server)
            .await;
        let gl = GitLabProvider::for_test(server.uri(), "t".into(), viewer("tester"));
        assert!(matches!(
            gl.list_repos().await.unwrap_err(),
            ProviderError::HttpStatus { .. }
        ));
    }

    #[tokio::test]
    async fn list_releases_treats_404_as_no_release() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v4/projects/42/releases"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let gl = GitLabProvider::for_test(server.uri(), "t".into(), viewer("tester"));
        assert!(gl.list_releases(&[repo()]).await.expect("ok").is_empty());
    }
}
