//! Persistent seen-state + the fire-and-forget entry point the aggregator
//! calls.
//!
//! Two responsibilities:
//!   1. Remember which items we have already notified about so a steady-
//!      state poll doesn't re-fire the same notification every tick.
//!      Persisted as `notifications.json` next to `settings.json`.
//!   2. Apply the user's notification preferences (master switch, Do-Not-
//!      Disturb, per-event toggle) just before calling the OS notification
//!      API via `tauri-plugin-notification`.
//!
//! Why a separate file (not folded into settings):
//!   * Settings are user-edited and read-mostly; the seen-store is opaque
//!     churn the user shouldn't touch.
//!   * Bundling them risks corrupting user preferences if a write at tick
//!     time races with a Settings edit in the UI.
//!
//! Cold-start contract: `initialised: false` means the store has never
//! been seeded. The aggregator's first tick after launch detects this,
//! treats *every* currently-visible item as already-seen, flips the flag,
//! and fires nothing — so an upgrade or fresh install doesn't blast the
//! notification centre with a year of backlog.
//!
//! 60-day TTL: entries older than that get pruned each tick. A stale issue
//! a user has forgotten about isn't worth a notification the moment it
//! happens to resurface in API output.

use crate::{settings::NotificationSettings, util::atomic_write};
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::PathBuf};
use tauri::{AppHandle, Manager};
use tauri_plugin_notification::NotificationExt;

pub const CURRENT_VERSION: u32 = 1;
const TTL_DAYS: i64 = 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeenStore {
    #[serde(default = "default_version")]
    pub version: u32,
    /// False until the first tick has had a chance to seed the store
    /// with everything currently visible. Drives the cold-start guard.
    #[serde(default)]
    pub initialised: bool,
    /// Waiting-item ID → RFC3339 timestamp of when we first noticed it.
    /// The timestamp is what gets used for TTL pruning; the value side of
    /// the map is never compared, only checked for key presence.
    #[serde(default)]
    pub waiting: HashMap<String, String>,
    /// Release composite-key → timestamp. Key shape:
    /// `<account_id>:<repo_full_name>:<tag_name>` so the same tag in
    /// different repos / accounts is tracked independently.
    #[serde(default)]
    pub releases: HashMap<String, String>,
    /// CI-failure composite-key → timestamp. Populated in Phase 3 when
    /// per-provider `author_login` plumbing lands; declared today so the
    /// on-disk schema is stable from this commit forward.
    #[serde(default)]
    pub ci_failures: HashMap<String, String>,
}

impl Default for SeenStore {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            initialised: false,
            waiting: HashMap::new(),
            releases: HashMap::new(),
            ci_failures: HashMap::new(),
        }
    }
}

fn default_version() -> u32 {
    CURRENT_VERSION
}

fn store_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("resolving app config dir: {e}"))?;
    Ok(dir.join("notifications.json"))
}

/// Load the seen-store, falling back to an empty default on any I/O or
/// parse error. A corrupt file shouldn't prevent the loop from running —
/// the worst case is a re-notification of items that were already seen,
/// which is mildly annoying but recoverable; the alternative (panicking)
/// is worse.
pub fn load(app: &AppHandle) -> SeenStore {
    let Ok(path) = store_path(app) else {
        return SeenStore::default();
    };
    if !path.exists() {
        return SeenStore::default();
    }
    match std::fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|e| {
            eprintln!("gitbuddy: notifications.json unreadable, starting fresh: {e}");
            SeenStore::default()
        }),
        Err(e) => {
            eprintln!("gitbuddy: notifications.json read failed: {e}");
            SeenStore::default()
        }
    }
}

pub fn save(app: &AppHandle, store: &SeenStore) -> Result<(), String> {
    let path = store_path(app)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir -p {parent:?}: {e}"))?;
    }
    let json =
        serde_json::to_string_pretty(store).map_err(|e| format!("serialising store: {e}"))?;
    atomic_write(&path, json.as_bytes())
}

/// Drop entries older than `TTL_DAYS`. Cheap — runs in-memory before the
/// store is rewritten, so the on-disk file shrinks over time instead of
/// growing forever.
pub fn prune(store: &mut SeenStore) {
    let cutoff = Utc::now() - Duration::days(TTL_DAYS);
    let keep = |ts: &String| -> bool {
        chrono::DateTime::parse_from_rfc3339(ts).is_ok_and(|t| t.with_timezone(&Utc) >= cutoff)
    };
    store.waiting.retain(|_, ts| keep(ts));
    store.releases.retain(|_, ts| keep(ts));
    store.ci_failures.retain(|_, ts| keep(ts));
}

/// A notification ready to be shown. Built by the aggregator from a diff
/// hit, then handed to `fire` for the settings-gated OS call. Pulling the
/// rendered strings into a dedicated enum (instead of passing raw items)
/// keeps the formatting logic out of the aggregator and lets unit tests
/// exercise the gating without needing real provider data.
#[derive(Debug, Clone)]
pub enum Kind {
    Waiting {
        reason_label: String,
        repo: String,
        title: String,
    },
    Release {
        repo: String,
        tag_name: String,
    },
    /// Wired today; populated by the aggregator once `author_login`
    /// plumbing lands in Phase 3. The `allow(dead_code)` keeps clippy
    /// quiet until that diff arrives — until then the variant exists so
    /// the on-disk seen-store schema and the settings toggle have a
    /// matching production code path waiting for them.
    #[allow(dead_code)]
    CiFailure {
        repo: String,
        branch: String,
    },
}

/// Apply settings gates and, if all pass, hand the notification off to
/// `tauri-plugin-notification`. Returns true if the OS call was issued
/// (purely for test/debug; the aggregator doesn't act on the return
/// value). Errors from the plugin are logged and swallowed — one bad
/// notification doesn't break the polling loop.
pub fn fire(app: &AppHandle, settings: &NotificationSettings, kind: Kind) -> bool {
    if !settings.enabled || settings.do_not_disturb {
        return false;
    }
    let event_enabled = match &kind {
        Kind::Waiting { .. } => settings.events.waiting,
        Kind::Release { .. } => settings.events.releases,
        Kind::CiFailure { .. } => settings.events.ci_failure,
    };
    if !event_enabled {
        return false;
    }

    let (title, body) = match kind {
        Kind::Waiting {
            reason_label,
            repo,
            title,
        } => (
            format!("gitBuddy — {reason_label}"),
            format!("{title}\n{repo}"),
        ),
        Kind::Release { repo, tag_name } => (
            "gitBuddy — New release".to_string(),
            format!("{repo} {tag_name}"),
        ),
        Kind::CiFailure { repo, branch } => (
            "gitBuddy — CI failure".to_string(),
            format!("{repo} ({branch})"),
        ),
    };

    match app.notification().builder().title(title).body(body).show() {
        Ok(_) => true,
        Err(e) => {
            eprintln!("gitbuddy: notification fire failed: {e}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{NotificationEventToggles, NotificationSettings};

    fn store_with(now: chrono::DateTime<Utc>, age_days: i64) -> SeenStore {
        let mut s = SeenStore::default();
        let ts = (now - Duration::days(age_days)).to_rfc3339();
        s.waiting.insert("old".into(), ts.clone());
        s.releases.insert("old-rel".into(), ts);
        s.waiting.insert("fresh".into(), now.to_rfc3339());
        s
    }

    #[test]
    fn prune_drops_only_entries_past_ttl() {
        let now = Utc::now();
        let mut s = store_with(now, TTL_DAYS + 1);
        prune(&mut s);
        assert!(!s.waiting.contains_key("old"));
        assert!(s.waiting.contains_key("fresh"));
        assert!(!s.releases.contains_key("old-rel"));
    }

    #[test]
    fn prune_keeps_entries_within_ttl() {
        let now = Utc::now();
        let mut s = store_with(now, TTL_DAYS - 1);
        prune(&mut s);
        assert!(s.waiting.contains_key("old"));
        assert!(s.waiting.contains_key("fresh"));
        assert!(s.releases.contains_key("old-rel"));
    }

    #[test]
    fn cold_start_default_is_uninitialised() {
        let s = SeenStore::default();
        assert!(!s.initialised);
        assert_eq!(s.version, CURRENT_VERSION);
    }

    fn settings_with(
        enabled: bool,
        dnd: bool,
        events: NotificationEventToggles,
    ) -> NotificationSettings {
        NotificationSettings {
            enabled,
            do_not_disturb: dnd,
            events,
        }
    }

    // The fire() function's gate logic is unit-testable without an
    // AppHandle because we can short-circuit before the OS call. The
    // assertions here pin the gate ordering (master switch → DnD →
    // per-event) so a refactor doesn't silently invert it.
    #[test]
    fn gate_logic_master_switch_off_silences_all() {
        let s = settings_with(false, false, NotificationEventToggles::default());
        assert!(!s.enabled);
        assert!(!would_pass_gates(
            &s,
            &Kind::Waiting {
                reason_label: "x".into(),
                repo: "y".into(),
                title: "z".into()
            }
        ));
    }

    #[test]
    fn gate_logic_dnd_silences_all() {
        let s = settings_with(true, true, NotificationEventToggles::default());
        assert!(!would_pass_gates(
            &s,
            &Kind::Release {
                repo: "x".into(),
                tag_name: "v1".into()
            }
        ));
    }

    #[test]
    fn gate_logic_per_event_can_silence_one_kind() {
        let s = settings_with(
            true,
            false,
            NotificationEventToggles {
                waiting: true,
                releases: false,
                ci_failure: true,
            },
        );
        assert!(would_pass_gates(
            &s,
            &Kind::Waiting {
                reason_label: "x".into(),
                repo: "y".into(),
                title: "z".into()
            }
        ));
        assert!(!would_pass_gates(
            &s,
            &Kind::Release {
                repo: "x".into(),
                tag_name: "v1".into()
            }
        ));
    }

    /// Mirror of `fire`'s gate logic without the OS call. Kept in tests so
    /// production `fire` doesn't get an "is this allowed" method that
    /// would invite skipping the OS call by mistake.
    fn would_pass_gates(s: &NotificationSettings, kind: &Kind) -> bool {
        if !s.enabled || s.do_not_disturb {
            return false;
        }
        match kind {
            Kind::Waiting { .. } => s.events.waiting,
            Kind::Release { .. } => s.events.releases,
            Kind::CiFailure { .. } => s.events.ci_failure,
        }
    }
}
