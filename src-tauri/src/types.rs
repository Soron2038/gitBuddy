//! Shared data types between the Rust providers and the Svelte frontend.
//!
//! All `serde` representations here are designed to round-trip cleanly with
//! the corresponding TypeScript types in `src/lib/data/stub.ts` — so swapping
//! the stub data for real provider output is a drop-in change for the UI.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Provider {
    Github,
    Gitlab,
    Codeberg,
    MpsdGitlab,
}

/// Kind of "waiting item" surfaced in the popover.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum ItemKind {
    /// Pull request (GitHub, Gitea, Codeberg).
    Pr,
    /// Merge request (GitLab).
    Mr,
    /// Issue (any forge).
    Is,
}

/// Why this item is showing up in the user's "waiting" view.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ItemReason {
    Assigned,
    Review,
    Authored,
    Mentioned,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaitingItem {
    pub id: String,
    pub kind: ItemKind,
    pub title: String,
    /// "owner/name"
    pub repo: String,
    pub provider: Provider,
    pub reason: ItemReason,
    pub url: String,
    /// Short human-readable age e.g. "2h", "1d", "3w" — computed server-side
    /// so the UI doesn't need to re-derive it on every render.
    pub age_human: String,
    /// Original RFC 3339 timestamp from the provider, kept for sorting and
    /// future re-derivation.
    pub updated_at: String,
    /// `Account.id` of the account that surfaced this item — set by the
    /// aggregator in `commands::list_waiting` after the provider returns,
    /// so providers stay account-agnostic. `None` only during construction
    /// inside a provider; the aggregator overwrites with `Some(...)` before
    /// the frontend ever sees the value.
    #[serde(default)]
    pub account_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Viewer {
    pub login: String,
    pub avatar_url: Option<String>,
    pub name: Option<String>,
}

/// How a given account authenticates against its provider. New OAuth-Device-
/// Flow accounts get `OauthDevice`; everything connected via the existing
/// "paste a token" flow is `Pat`. The frontend renders a small badge per
/// account row so the user can see which method is in use.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    Pat,
    OauthDevice,
}

/// One connected account. The on-disk `accounts.json` holds a `Vec<Account>`;
/// the actual secret material (PAT string or OAuth tokens blob) lives in the
/// Keychain under the composite key `Account.id`.
///
/// `id` is built from `<provider-slug>:<login-lowercased>` so it round-trips
/// stably and serves as both the in-memory primary key and the Keychain
/// account name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub provider: Provider,
    pub login: String,
    pub viewer: Viewer,
    pub auth: AuthMethod,
    /// Base URL for self-hostable providers (GitLab/Codeberg). `None` for
    /// GitHub.com.
    #[serde(default)]
    pub base_url: Option<String>,
    /// RFC 3339 timestamp captured when the account was first added — used
    /// for stable display ordering ("most recently added") in later
    /// multi-account UI work.
    pub added_at: String,
}

/// Coarse CI state collapsed from the provider's richer status/conclusion
/// matrix into the four buckets the UI cares about: green / red / spinning /
/// nothing.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CiStatus {
    /// Workflow completed successfully.
    Ok,
    /// Workflow failed, timed out, or requires action.
    Fail,
    /// Currently queued or running.
    Run,
    /// Cancelled or skipped — not a failure but not a pass either.
    Cancelled,
    /// Repo has no CI configured, or no workflow runs found.
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiRun {
    pub repo_id: String,
    /// "owner/name" — useful when the frontend wants to render CI rows
    /// independently of the repo list.
    pub repo_full_name: String,
    pub status: CiStatus,
    pub html_url: Option<String>,
    pub branch: Option<String>,
    pub workflow_name: Option<String>,
    /// Login of the user that *triggered* the latest run on this repo.
    /// Aggregator compares it (lowercased) against the connected account's
    /// viewer login to decide whether to fire a "your CI failed"
    /// notification. `None` when the provider doesn't surface an actor
    /// (some self-hosted Forgejo instances) — in that case the CI-failure
    /// notification path is silently skipped for that repo.
    #[serde(default)]
    pub author_login: Option<String>,
    /// `Account.id` of the providing account. Same aggregator-tagging
    /// contract as [`WaitingItem::account_id`].
    #[serde(default)]
    pub account_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Release {
    pub repo_id: String,
    /// "owner/name" for display.
    pub repo_full_name: String,
    pub provider: Provider,
    pub tag: String,
    /// Release title; falls back to the tag if the publisher didn't set one.
    pub name: String,
    pub published_at: String,
    pub html_url: String,
    pub is_prerelease: bool,
    /// True when published within the last 7 days — drives the "NEW" badge.
    pub is_new: bool,
    /// Pre-rendered relative age, e.g. "2d", "3w".
    pub age_human: String,
    /// `Account.id` of the providing account. Same aggregator-tagging
    /// contract as [`WaitingItem::account_id`].
    #[serde(default)]
    pub account_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repo {
    /// Provider-stable identifier (e.g. GitHub numeric `id` as a string).
    pub id: String,
    pub owner: String,
    pub name: String,
    pub provider: Provider,
    pub default_branch: String,
    pub language: Option<String>,
    pub description: Option<String>,
    pub stars: u64,
    pub html_url: String,
    /// One of the SSH clone URLs the provider exposes, kept for the M3
    /// local-clone matcher.
    pub ssh_url: Option<String>,
    /// The HTTPS clone URL — same purpose.
    pub clone_url: Option<String>,
    pub is_fork: bool,
    pub is_private: bool,
    /// RFC 3339 timestamp of the most recent push to the default branch,
    /// used purely for sorting the repo list by recency.
    pub pushed_at: Option<String>,
    /// `Account.id` of the providing account. Same aggregator-tagging
    /// contract as [`WaitingItem::account_id`]. When a repo is visible
    /// through multiple accounts (e.g. cross-org membership), the
    /// per-account rows produced here are deduped at the next layer up.
    #[serde(default)]
    pub account_id: Option<String>,
}
