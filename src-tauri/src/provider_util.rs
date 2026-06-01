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

use crate::types::{CiRun, CiStatus, ItemReason, Release, Repo, Viewer, WaitingItem};
use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use thiserror::Error;

/// One error type for every forge provider. Before this existed, each
/// provider carried a near-identical `GitHubError` / `GitLabError` /
/// `CodebergError`; the only real differences were the auth-scope hint and
/// whether the HTTP error carried a base URL. Unifying them lets the
/// aggregator and command layer handle a single `Result<_, ProviderError>`.
#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
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
}

/// The behaviour every forge provider implements. Construction stays an
/// inherent `connect` on each concrete type (its signature differs per
/// provider and it returns `Self`, so it can't live on an object-safe
/// trait); everything the aggregator and commands need at runtime goes
/// here, so they can hold `Arc<dyn ProviderBackend>` instead of three
/// concrete provider maps.
#[async_trait::async_trait]
pub trait ProviderBackend: Send + Sync {
    /// The authenticated account behind this provider.
    fn viewer(&self) -> &Viewer;
    /// The bearer token, needed for outbound git operations (clone).
    fn token(&self) -> &str;
    /// The forge base URL, or `None` for GitHub (always api.github.com).
    fn base_url(&self) -> Option<&str>;

    async fn list_waiting(&self) -> Result<Vec<WaitingItem>, ProviderError>;
    async fn list_repos(&self) -> Result<Vec<Repo>, ProviderError>;
    async fn list_releases(&self) -> Result<Vec<Release>, ProviderError>;
    async fn list_ci(&self) -> Result<Vec<CiRun>, ProviderError>;
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
