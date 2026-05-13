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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Viewer {
    pub login: String,
    pub avatar_url: Option<String>,
    pub name: Option<String>,
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
}
