//! GitHub provider — PAT-based authentication, viewer info, and the
//! "waiting on me" search across assigned/review-requested/authored/mentioned.
//!
//! M2 deliberately stays close to GitHub's REST search API. GraphQL would
//! consolidate the four queries into one, but the search API works fine for
//! single-digit account counts and avoids hand-rolling a GraphQL client for
//! milestone one of the providers.

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

const API_BASE: &str = "https://api.github.com";
const ACCEPT: &str = "application/vnd.github+json";

/// Hint surfaced when GitHub rejects the token — names the scope to check.
const AUTH_HINT: &str = "check that the token is valid and has `repo` scope";

pub type Result<T> = std::result::Result<T, ProviderError>;

pub struct GitHubProvider {
    client: Client,
    token: String,
    /// API base, normally [`API_BASE`]. Held as a field (rather than using the
    /// const directly) so tests can point the provider at a mock server via
    /// [`Self::for_test`]; production always passes `API_BASE`.
    api_base: String,
    pub viewer: Viewer,
}

impl GitHubProvider {
    /// Construct a provider from a personal access token, verifying that the
    /// token works by hitting `/user`. Returns the authenticated viewer so
    /// callers can show a "connected as @login" confirmation.
    pub async fn connect(token: String) -> Result<Self> {
        let client = http_client()?;
        let viewer = fetch_viewer(&client, &token, API_BASE).await?;
        Ok(Self {
            client,
            token,
            api_base: API_BASE.to_string(),
            viewer,
        })
    }

    /// Construct a provider pointed at an arbitrary API base (a mock server),
    /// skipping the `/user` round-trip that [`Self::connect`] performs. Tests
    /// only — the HTTP-conformance suite drives the real request paths against
    /// a localhost `wiremock` server.
    #[cfg(test)]
    pub(crate) fn for_test(api_base: String, token: String, viewer: Viewer) -> Self {
        Self {
            client: http_client().expect("test http client"),
            token,
            api_base,
            viewer,
        }
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
            let base = self.api_base.clone();
            handles.push(tokio::spawn(async move {
                search_issues(&client, &token, &base, &q, reason).await
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
                .get(format!("{}/user/repos", self.api_base))
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
                StatusCode::UNAUTHORIZED => return Err(ProviderError::Unauthorized(AUTH_HINT)),
                s => return Err(http_error("GitHub", None, s)),
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

    /// Latest release per repo, for the N most-recently-pushed of the given
    /// `repos` (the aggregator's once-per-tick `list_repos` result). Bounded
    /// because /releases/latest is one call per repo and we don't want to
    /// spend the rate-limit budget on dormant archives.
    pub async fn list_releases(&self, repos: &[Repo]) -> Result<Vec<Release>> {
        const MAX_REPOS_TO_CHECK: usize = 60;

        let repos: Vec<Repo> = repos.iter().take(MAX_REPOS_TO_CHECK).cloned().collect();

        let mut handles = Vec::with_capacity(repos.len());
        for repo in repos {
            let client = self.client.clone();
            let token = self.token.clone();
            let base = self.api_base.clone();
            handles.push(tokio::spawn(async move {
                fetch_latest_release(&client, &token, &base, &repo).await
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
    /// recently-pushed of the given `repos`. Used to paint a coloured status
    /// dot next to each repo row in the UI.
    pub async fn list_ci(&self, repos: &[Repo]) -> Result<Vec<CiRun>> {
        const MAX_REPOS_TO_CHECK: usize = 60;

        let repos: Vec<Repo> = repos.iter().take(MAX_REPOS_TO_CHECK).cloned().collect();

        let mut handles = Vec::with_capacity(repos.len());
        for repo in repos {
            let client = self.client.clone();
            let token = self.token.clone();
            let base = self.api_base.clone();
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
impl ProviderBackend for GitHubProvider {
    fn viewer(&self) -> &Viewer {
        &self.viewer
    }
    fn token(&self) -> &str {
        &self.token
    }
    fn base_url(&self) -> Option<&str> {
        None
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

/// Top-level wrapper of the `/repos/{owner}/{name}/actions/runs` response.
/// Hoisted to module level so the deserializer can be exercised by unit
/// tests against recorded fixtures without spinning up a real HTTP client.
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
    /// User who triggered the run (push committer, PR author, or whoever
    /// clicked "Re-run"). The notifications pipeline only fires
    /// CI-failure events when this matches the connected account's
    /// viewer login — see `aggregator::diff_and_notify`. On a re-run the
    /// actor is the person who clicked the button, not the original
    /// author; accepted edge case (DECISIONS.md 2026-05-26).
    #[serde(default)]
    actor: Option<Actor>,
}

#[derive(Deserialize)]
struct Actor {
    login: String,
}

async fn fetch_latest_ci_run(
    client: &Client,
    token: &str,
    base: &str,
    repo: &Repo,
) -> Result<Option<CiRun>> {
    let url = format!("{base}/repos/{}/{}/actions/runs", repo.owner, repo.name);
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
                author_login: None,
                account_id: None,
            }));
        }
        StatusCode::UNAUTHORIZED => return Err(ProviderError::Unauthorized(AUTH_HINT)),
        // 403 can happen when the repo's owner has disabled actions for
        // forks; treat it the same as "no CI" to keep the batch flowing.
        StatusCode::FORBIDDEN => return Ok(None),
        s => return Err(http_error("GitHub", None, s)),
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

async fn fetch_latest_release(
    client: &Client,
    token: &str,
    base: &str,
    repo: &Repo,
) -> Result<Option<Release>> {
    let url = format!("{base}/repos/{}/{}/releases/latest", repo.owner, repo.name);
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
        StatusCode::UNAUTHORIZED => return Err(ProviderError::Unauthorized(AUTH_HINT)),
        s => return Err(http_error("GitHub", None, s)),
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
        account_id: None,
    }))
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
            account_id: None,
        }
    }
}

async fn fetch_viewer(client: &Client, token: &str, base: &str) -> Result<Viewer> {
    let resp = client
        .get(format!("{base}/user"))
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
        StatusCode::UNAUTHORIZED => Err(ProviderError::Unauthorized(AUTH_HINT)),
        s => Err(http_error("GitHub", None, s)),
    }
}

async fn search_issues(
    client: &Client,
    token: &str,
    base: &str,
    q: &str,
    reason: ItemReason,
) -> Result<Vec<WaitingItem>> {
    let resp = client
        .get(format!("{base}/search/issues"))
        .bearer_auth(token)
        .header("Accept", ACCEPT)
        .query(&[("q", q), ("per_page", "50")])
        .send()
        .await?;

    match resp.status() {
        s if s.is_success() => {}
        StatusCode::UNAUTHORIZED => return Err(ProviderError::Unauthorized(AUTH_HINT)),
        s => return Err(http_error("GitHub", None, s)),
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
            account_id: None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_util::test_support::{json_array, viewer};
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn parses_repo_from_api_url() {
        assert_eq!(
            parse_repo("https://api.github.com/repos/anthropics/claude-code"),
            "anthropics/claude-code"
        );
    }

    #[test]
    fn workflow_run_extracts_actor_login() {
        // Trimmed fixture from a real `/actions/runs` response — keeps
        // only the fields the deserializer cares about.
        let raw = r#"{
            "workflow_runs": [{
                "status": "completed",
                "conclusion": "failure",
                "html_url": "https://github.com/o/r/actions/runs/42",
                "head_branch": "main",
                "name": "CI",
                "actor": {"login": "Soron2038", "id": 99}
            }]
        }"#;
        let resp: WorkflowRunsResp = serde_json::from_str(raw).expect("parse");
        let run = resp.workflow_runs.into_iter().next().unwrap();
        assert_eq!(run.actor.map(|a| a.login).as_deref(), Some("Soron2038"));
    }

    #[test]
    fn workflow_run_actor_optional() {
        // GitHub's documented schema lists `actor` as always-present, but
        // we keep `Option<Actor>` so a future shape change doesn't make
        // the entire deserialisation fail — the CI-failure path simply
        // skips runs with no actor.
        let raw = r#"{
            "workflow_runs": [{
                "status": "completed",
                "conclusion": "failure",
                "html_url": "https://github.com/o/r/actions/runs/43",
                "head_branch": "main",
                "name": "CI"
            }]
        }"#;
        let resp: WorkflowRunsResp = serde_json::from_str(raw).expect("parse");
        let run = resp.workflow_runs.into_iter().next().unwrap();
        assert!(run.actor.is_none());
    }

    // ---- HTTP-conformance suite ------------------------------------------
    // Drives the real reqwest request paths against a localhost wiremock
    // server (via `for_test`), covering pagination, the bearer header, the
    // rate-limit/error/404 mappings, and `list_waiting`'s fail-soft logic.

    /// A `/user/repos` element carrying only the fields `RawRepo` requires.
    fn repo_json(i: usize) -> String {
        format!(
            r#"{{"id":{i},"name":"r{i}","owner":{{"login":"o"}},"stargazers_count":0,"html_url":"https://x/{i}","fork":false,"private":false}}"#
        )
    }

    fn repo(owner: &str, name: &str) -> Repo {
        Repo {
            id: "1".into(),
            owner: owner.into(),
            name: name.into(),
            provider: Provider::Github,
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
        // Page 1 is full (PAGE_SIZE = 100) → the loop fetches page 2…
        Mock::given(method("GET"))
            .and(path("/user/repos"))
            .and(query_param("page", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_string(json_array(100, repo_json)))
            .expect(1)
            .mount(&server)
            .await;
        // …page 2 is short (< 100) → it stops, so page 3 is never requested
        // (an unmounted page 3 would 404 and surface as an error instead).
        Mock::given(method("GET"))
            .and(path("/user/repos"))
            .and(query_param("page", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_string(json_array(1, repo_json)))
            .expect(1)
            .mount(&server)
            .await;

        let gh = GitHubProvider::for_test(server.uri(), "t".into(), viewer("tester"));
        assert_eq!(gh.list_repos().await.expect("ok").len(), 101);
    }

    #[tokio::test]
    async fn list_repos_sends_bearer_token() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user/repos"))
            .and(header("authorization", "Bearer testtoken"))
            .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
            .expect(1)
            .mount(&server)
            .await;

        let gh = GitHubProvider::for_test(server.uri(), "testtoken".into(), viewer("tester"));
        // Succeeds only if the bearer header matched; otherwise no mock matches
        // and the 404 would surface as an error.
        gh.list_repos().await.expect("authorised");
    }

    #[tokio::test]
    async fn list_repos_maps_401_to_unauthorized() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user/repos"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let gh = GitHubProvider::for_test(server.uri(), "t".into(), viewer("tester"));
        assert!(matches!(
            gh.list_repos().await.unwrap_err(),
            ProviderError::Unauthorized(_)
        ));
    }

    #[tokio::test]
    async fn list_repos_maps_429_to_rate_limited() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user/repos"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;
        let gh = GitHubProvider::for_test(server.uri(), "t".into(), viewer("tester"));
        assert!(matches!(
            gh.list_repos().await.unwrap_err(),
            ProviderError::RateLimited { .. }
        ));
    }

    #[tokio::test]
    async fn list_repos_maps_5xx_to_http_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user/repos"))
            .respond_with(ResponseTemplate::new(502))
            .mount(&server)
            .await;
        let gh = GitHubProvider::for_test(server.uri(), "t".into(), viewer("tester"));
        assert!(matches!(
            gh.list_repos().await.unwrap_err(),
            ProviderError::HttpStatus { .. }
        ));
    }

    #[tokio::test]
    async fn list_releases_treats_404_as_no_release() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/releases/latest"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let gh = GitHubProvider::for_test(server.uri(), "t".into(), viewer("tester"));
        assert!(gh
            .list_releases(&[repo("o", "r")])
            .await
            .expect("ok")
            .is_empty());
    }

    #[tokio::test]
    async fn list_waiting_tolerates_a_failing_scope() {
        let server = MockServer::start().await;
        // The four search scopes run concurrently; the "author" one 500s while
        // the other three each return a distinct item.
        let ok_scopes = [
            ("is:open assignee:tester archived:false", 1, "o/r1"),
            ("is:open review-requested:tester archived:false", 2, "o/r2"),
            ("is:open mentions:tester archived:false", 3, "o/r3"),
        ];
        for (q, id, full) in ok_scopes {
            let body = format!(
                r#"{{"items":[{{"id":{id},"title":"t","html_url":"https://x/{id}","updated_at":"2026-06-01T00:00:00Z","repository_url":"https://api.github.com/repos/{full}"}}]}}"#
            );
            Mock::given(method("GET"))
                .and(path("/search/issues"))
                .and(query_param("q", q))
                .respond_with(ResponseTemplate::new(200).set_body_string(body))
                .mount(&server)
                .await;
        }
        Mock::given(method("GET"))
            .and(path("/search/issues"))
            .and(query_param("q", "is:open author:tester archived:false"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let gh = GitHubProvider::for_test(server.uri(), "t".into(), viewer("tester"));
        let items = gh
            .list_waiting()
            .await
            .expect("one failing scope must not blank the whole list");
        assert_eq!(items.len(), 3);
    }

    #[tokio::test]
    async fn list_waiting_propagates_401() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search/issues"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let gh = GitHubProvider::for_test(server.uri(), "t".into(), viewer("tester"));
        assert!(matches!(
            gh.list_waiting().await.unwrap_err(),
            ProviderError::Unauthorized(_)
        ));
    }
}
