//! Shared building blocks for the per-forge provider modules.
//!
//! Before this module existed, `github.rs`, `gitlab.rs`, and `codeberg.rs`
//! each carried byte-identical copies of these helpers. They map a forge's
//! raw timestamps / reasons / CI vocabulary onto the shared types in
//! `types.rs`, so they belong with the providers but not inside any one of
//! them.
//!
//! Note GitLab intentionally does *not* use [`collapse_ci_status`] — its
//! pipeline status vocabulary differs from GitHub Actions, so it keeps its
//! own `collapse_pipeline_status` in `gitlab.rs`.

use crate::types::{CiRun, CiStatus, ItemReason, Release, Repo, WaitingItem};
use chrono::{DateTime, Utc};
use reqwest::header::HeaderMap;
use reqwest::{Client, StatusCode};
use std::time::Duration;
use thiserror::Error;

/// Shared User-Agent for every outbound HTTP request (providers + OAuth).
pub(crate) const USER_AGENT: &str = concat!("gitBuddy/", env!("CARGO_PKG_VERSION"));

/// Build the reqwest client every provider (and the OAuth flow) uses. The
/// deadlines matter: without them a host that accepts the TCP connection but
/// never answers would hang its fetch — and the aggregator tick awaiting it —
/// forever.
pub(crate) fn http_client() -> reqwest::Result<Client> {
    Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
}

/// One error type for every forge provider. Before this existed, each
/// provider carried a near-identical `GitHubError` / `GitLabError` /
/// `CodebergError`; the only real differences were the auth-scope hint and
/// whether the HTTP error carried a base URL. Unifying them lets the
/// aggregator and command layer handle a single `Result<_, ProviderError>`.
#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    /// The request succeeded but the body wasn't what we expected. Split out
    /// from `Network` because `reqwest`'s decode errors funnel through the
    /// same type: a self-hosted instance behind an SSO proxy answers 200 with
    /// an HTML login page, and reporting that as "network error: error
    /// decoding response body" sends the user off to debug their Wi-Fi.
    #[error("{provider} returned an unexpected response body — is the base URL pointing at the API? ({source})")]
    Decode {
        provider: &'static str,
        source: reqwest::Error,
    },
    /// `.0` is a provider-specific hint naming the token scopes to check.
    #[error("authentication failed — {0}")]
    Unauthorized(&'static str),
    /// `base_url` is `None` for GitHub (single host) and `Some` for the
    /// self-hostable forges, reproducing the original
    /// "{provider} API[ at {base_url}] returned HTTP {status}" wording.
    #[error("{provider} API{} returned HTTP {status}", base_url.as_deref().map(|u| format!(" at {u}")).unwrap_or_default())]
    HttpStatus {
        provider: &'static str,
        base_url: Option<String>,
        status: StatusCode,
    },
    #[error("invalid base URL: {0}")]
    InvalidBaseUrl(String),
    /// The forge is throttling us. Distinct from `HttpStatus` so the
    /// aggregator can name the condition in `last_error` (instead of a bare
    /// status code) and a backoff can key on it. `retry_after_secs` carries
    /// the server's own hint when it sent one.
    #[error("{provider} is rate-limiting requests{} — backing off", retry_after_secs.map(|s| format!(" (retry in ~{s}s)")).unwrap_or_default())]
    RateLimited {
        provider: &'static str,
        retry_after_secs: Option<u64>,
    },
}

/// Is this response a rate-limit rejection?
///
/// A plain 429 is the easy case. The hard one is GitHub, which answers **403**
/// for both primary-limit exhaustion and secondary/abuse limits — the same
/// status several endpoints legitimately use for "this feature is disabled for
/// this repo". The headers disambiguate: `x-ratelimit-remaining: 0` or a
/// `retry-after` means throttling, anything else means the feature reading.
/// Getting this wrong is not cosmetic — a throttled 403 read as "no CI" makes
/// the app quietly show empty lists while it is being told to slow down.
fn rate_limit_hint(status: StatusCode, headers: &HeaderMap) -> Option<Option<u64>> {
    let retry_after = headers
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());

    if status == StatusCode::TOO_MANY_REQUESTS {
        return Some(retry_after);
    }
    if status == StatusCode::FORBIDDEN {
        let exhausted = headers
            .get("x-ratelimit-remaining")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.trim() == "0");
        if exhausted || retry_after.is_some() {
            // Prefer the explicit retry-after; otherwise derive the wait from
            // the reset epoch if the forge sent one.
            let secs = retry_after.or_else(|| {
                let reset = headers
                    .get("x-ratelimit-reset")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.trim().parse::<i64>().ok())?;
                let now = Utc::now().timestamp();
                (reset > now).then(|| (reset - now) as u64)
            });
            return Some(secs);
        }
    }
    None
}

/// How many per-repo HTTP calls one account may have in flight at once.
///
/// The release and CI lookups each fan out over up to `MAX_REPOS_TO_CHECK`
/// repos, and the aggregator runs both concurrently — so an unthrottled
/// provider put 120 simultaneous requests on the wire per account, times the
/// number of connected accounts. GitHub's own guidance is to stay at or below
/// 100 concurrent requests; past that it answers 403 (secondary rate limit).
/// Against a small self-hosted Forgejo, a burst that size every poll interval
/// is a self-inflicted DoS. Both fan-outs share one budget per account, which
/// is why the semaphore lives on the provider rather than in each call.
pub(crate) const MAX_CONCURRENT_REPO_CALLS: usize = 6;

/// A fresh per-repo call budget. One per provider instance.
pub(crate) fn repo_call_budget() -> std::sync::Arc<tokio::sync::Semaphore> {
    std::sync::Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_REPO_CALLS))
}

/// Wrap a body-decode failure with the provider that produced it.
pub(crate) fn decode_error(provider: &'static str) -> impl Fn(reqwest::Error) -> ProviderError {
    move |source| ProviderError::Decode { provider, source }
}

/// True when this response is a throttle rejection rather than a genuine
/// per-endpoint permission answer. Call sites that treat 403 as a graceful
/// "feature disabled / nothing visible here" must guard on this first,
/// otherwise a throttled account degrades into empty lists with no error.
pub(crate) fn is_rate_limited(status: StatusCode, headers: &HeaderMap) -> bool {
    rate_limit_hint(status, headers).is_some()
}

/// Classify a non-success response into the right `ProviderError`.
///
/// Takes the whole response rather than a bare status because both
/// interesting cases need the headers: rate limiting (see
/// [`rate_limit_hint`]) and 403-as-auth-failure. 403 is what GitHub returns
/// for a PAT that hasn't been SSO-authorised for an org and what GitLab
/// returns for `insufficient_scope` — mapping it to `Unauthorized` is what
/// gets the provider's scope hint in front of the user instead of a bare
/// "HTTP 403".
pub(crate) fn response_error(
    provider: &'static str,
    base_url: Option<String>,
    auth_hint: &'static str,
    status: StatusCode,
    headers: &HeaderMap,
) -> ProviderError {
    if let Some(retry_after_secs) = rate_limit_hint(status, headers) {
        return ProviderError::RateLimited {
            provider,
            retry_after_secs,
        };
    }
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return ProviderError::Unauthorized(auth_hint);
    }
    ProviderError::HttpStatus {
        provider,
        base_url,
        status,
    }
}

/// Compare two provider timestamps newest-first.
///
/// The three forges do not agree on a wire format: GitHub emits
/// `2026-06-01T12:00:00Z`, GitLab appends milliseconds (`…:00.000Z`), and
/// Gitea/Forgejo render RFC 3339 in the *instance's* configured timezone
/// (`…T14:00:00+02:00`). A lexicographic `String::cmp` is only correct for a
/// single normalised format, so a Codeberg item at `14:00:00+02:00` (= 12:00
/// UTC) would sort above a GitHub item at `13:00:00Z` an hour later in real
/// time. Parsing to `DateTime<Utc>` first fixes both the offset and the
/// millisecond case (`'.' < 'Z'`).
///
/// Unparseable values sort last rather than poisoning the order, and two
/// unparseable values fall back to a string comparison so the sort stays
/// total (a non-total comparator would make `sort_by` misbehave).
pub(crate) fn cmp_newest_first(a: &str, b: &str) -> std::cmp::Ordering {
    match (
        DateTime::parse_from_rfc3339(a),
        DateTime::parse_from_rfc3339(b),
    ) {
        (Ok(a), Ok(b)) => b.with_timezone(&Utc).cmp(&a.with_timezone(&Utc)),
        (Ok(_), Err(_)) => std::cmp::Ordering::Less,
        (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
        (Err(_), Err(_)) => b.cmp(a),
    }
}

/// Normalise a user-supplied forge base URL: trim whitespace and trailing
/// slashes, reject empty input. HTTPS-only — the base URL is the channel the
/// PAT travels on for every API call, and `http://` would send the bearer
/// token in clear if the user pastes (or is phished into) a plain-HTTP host.
/// If a localhost dev-instance ever needs `http://`, gate it explicitly on
/// `localhost` / `127.0.0.1` / `::1` then. Shared by the self-hostable
/// forges (GitLab, Gitea/Forgejo); GitHub has no base URL.
pub(crate) fn normalise_base_url(raw: &str) -> Result<String, ProviderError> {
    let trimmed = raw.trim().trim_end_matches('/').to_string();
    if trimmed.is_empty() {
        return Err(ProviderError::InvalidBaseUrl(
            "base URL must not be empty".into(),
        ));
    }
    if !trimmed.starts_with("https://") {
        return Err(ProviderError::InvalidBaseUrl(format!(
            "base URL must start with https://: {trimmed}"
        )));
    }
    Ok(trimmed)
}

/// The behaviour every forge provider implements. Construction stays an
/// inherent `connect` on each concrete type (its signature differs per
/// provider and it returns `Self`, so it can't live on an object-safe
/// trait); everything the aggregator and commands need at runtime goes
/// here, so they can hold `Arc<dyn ProviderBackend>` instead of three
/// concrete provider maps.
#[async_trait::async_trait]
pub trait ProviderBackend: Send + Sync {
    /// The bearer token, needed for outbound git operations (clone).
    fn token(&self) -> &str;
    /// The forge base URL, or `None` for GitHub (always api.github.com).
    fn base_url(&self) -> Option<&str>;

    async fn list_waiting(&self) -> Result<Vec<WaitingItem>, ProviderError>;
    async fn list_repos(&self) -> Result<Vec<Repo>, ProviderError>;
    /// `repos` is the result of one `list_repos` call per tick, passed in by
    /// the aggregator. Releases/CI used to re-fetch the repo list internally,
    /// which tripled the API spend per tick (pagination included) for no
    /// fresher data.
    async fn list_releases(&self, repos: &[Repo]) -> Result<Vec<Release>, ProviderError>;
    /// See [`Self::list_releases`] for the `repos` contract.
    async fn list_ci(&self, repos: &[Repo]) -> Result<Vec<CiRun>, ProviderError>;
}

/// Render an RFC3339 timestamp as a compact relative age ("now", "30m",
/// "4h", "3d", "2mo", "1y"). Returns "?" if the timestamp doesn't parse.
pub(crate) fn humanise_age(ts: &str, now: DateTime<Utc>) -> String {
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

/// Lower number = higher priority. Used to keep the most actionable reason
/// when the same item surfaces under multiple "waiting" scopes.
pub(crate) fn reason_priority(r: ItemReason) -> u8 {
    match r {
        ItemReason::Assigned => 0,
        ItemReason::Review => 1,
        ItemReason::Authored => 2,
        ItemReason::Mentioned => 3,
    }
}

/// Whether an RFC3339 timestamp is at most `days` old relative to `now`.
/// Returns false for unparseable input.
pub(crate) fn within_days(timestamp: &str, now: &DateTime<Utc>, days: i64) -> bool {
    DateTime::parse_from_rfc3339(timestamp)
        .map(|t| (*now - t.with_timezone(&Utc)).num_days() <= days)
        .unwrap_or(false)
}

/// Collapse GitHub Actions' status × conclusion matrix into our four-state
/// enum. `status` is one of queued / in_progress / completed; `conclusion`
/// is only meaningful when status is completed. Gitea/Forgejo Actions reuse
/// the same vocabulary, so Codeberg shares this; GitLab pipelines do not.
pub(crate) fn collapse_ci_status(status: &str, conclusion: Option<&str>) -> CiStatus {
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

/// Shared helpers for the per-provider HTTP-conformance suites. Lives here
/// (rather than duplicated per module) because all three providers build the
/// same kind of fixtures against a `wiremock` server. `pub(crate)` + `cfg(test)`
/// so each provider's `#[cfg(test)] mod tests` can reach it crate-wide.
#[cfg(test)]
pub(crate) mod test_support {
    use crate::types::Viewer;

    /// A minimal viewer stub for `Provider::for_test` — providers only read
    /// `viewer.login` (GitHub's `list_waiting` search queries), never the rest.
    pub(crate) fn viewer(login: &str) -> Viewer {
        Viewer {
            login: login.to_string(),
            avatar_url: None,
            name: None,
        }
    }

    /// Build a JSON array of `n` objects, each rendered by `make(index)`. Used
    /// to synthesise a full pagination page (`PAGE_SIZE` items) without pasting
    /// a hundred near-identical objects into the test.
    pub(crate) fn json_array(n: usize, make: impl Fn(usize) -> String) -> String {
        let items: Vec<String> = (0..n).map(make).collect();
        format!("[{}]", items.join(","))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn humanises_age_buckets() {
        let now = DateTime::parse_from_rfc3339("2026-05-12T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(humanise_age("2026-05-12T11:30:00Z", now), "30m");
        assert_eq!(humanise_age("2026-05-12T08:00:00Z", now), "4h");
        assert_eq!(humanise_age("2026-05-09T12:00:00Z", now), "3d");
        assert_eq!(humanise_age("not-a-timestamp", now), "?");
    }

    #[test]
    fn within_days_bounds() {
        let now = DateTime::parse_from_rfc3339("2026-05-12T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert!(within_days("2026-05-09T12:00:00Z", &now, 7));
        assert!(!within_days("2026-04-01T12:00:00Z", &now, 7));
        assert!(!within_days("garbage", &now, 7));
    }

    #[test]
    fn normalise_base_url_trims_and_enforces_https() {
        assert_eq!(
            normalise_base_url("https://codeberg.org/").unwrap(),
            "https://codeberg.org"
        );
        assert_eq!(
            normalise_base_url("  https://gitlab.gwdg.de/  ").unwrap(),
            "https://gitlab.gwdg.de"
        );
        assert!(normalise_base_url("http://gitlab.example.com").is_err());
        assert!(normalise_base_url("gitlab.example.com").is_err());
        assert!(normalise_base_url("   ").is_err());
    }

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                reqwest::header::HeaderName::from_bytes(k.as_bytes()).expect("header name"),
                v.parse().expect("header value"),
            );
        }
        h
    }

    fn classify(status: StatusCode, hdrs: &[(&str, &str)]) -> ProviderError {
        response_error("GitHub", None, "hint", status, &headers(hdrs))
    }

    #[test]
    fn plain_429_is_rate_limited_and_carries_retry_after() {
        assert!(matches!(
            classify(StatusCode::TOO_MANY_REQUESTS, &[]),
            ProviderError::RateLimited {
                retry_after_secs: None,
                ..
            }
        ));
        assert!(matches!(
            classify(StatusCode::TOO_MANY_REQUESTS, &[("retry-after", "60")]),
            ProviderError::RateLimited {
                retry_after_secs: Some(60),
                ..
            }
        ));
    }

    #[test]
    fn exhausted_403_is_rate_limited_not_a_disabled_feature() {
        // GitHub answers 403 for primary-limit exhaustion. Reading that as
        // "Actions disabled for this repo" is what made the app show empty
        // CI/release lists while it was being throttled.
        assert!(matches!(
            classify(StatusCode::FORBIDDEN, &[("x-ratelimit-remaining", "0")]),
            ProviderError::RateLimited { .. }
        ));
        assert!(matches!(
            classify(StatusCode::FORBIDDEN, &[("retry-after", "30")]),
            ProviderError::RateLimited {
                retry_after_secs: Some(30),
                ..
            }
        ));
    }

    #[test]
    fn plain_403_is_an_auth_problem_and_surfaces_the_scope_hint() {
        // A PAT that hasn't been SSO-authorised for an org, or one missing a
        // scope — the user needs the hint, not "HTTP 403".
        let e = classify(StatusCode::FORBIDDEN, &[("x-ratelimit-remaining", "4999")]);
        assert!(matches!(e, ProviderError::Unauthorized("hint")));
        assert!(e.to_string().contains("hint"));

        assert!(matches!(
            classify(StatusCode::UNAUTHORIZED, &[]),
            ProviderError::Unauthorized(_)
        ));
    }

    #[test]
    fn other_statuses_stay_generic() {
        assert!(matches!(
            response_error(
                "GitLab",
                Some("https://gitlab.com".into()),
                "hint",
                StatusCode::BAD_GATEWAY,
                &HeaderMap::new(),
            ),
            ProviderError::HttpStatus { .. }
        ));
    }

    #[test]
    fn collapses_ci_status_matrix() {
        assert_eq!(collapse_ci_status("in_progress", None), CiStatus::Run);
        assert_eq!(
            collapse_ci_status("completed", Some("success")),
            CiStatus::Ok
        );
        assert_eq!(
            collapse_ci_status("completed", Some("failure")),
            CiStatus::Fail
        );
        assert_eq!(
            collapse_ci_status("completed", Some("cancelled")),
            CiStatus::Cancelled
        );
        assert_eq!(collapse_ci_status("completed", None), CiStatus::None);
    }
}
