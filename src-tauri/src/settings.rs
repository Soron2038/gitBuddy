//! Persistent user settings stored as JSON inside the OS-standard application
//! support directory (e.g. `~/Library/Application Support/dev.soron2038.gitbuddy/`).
//!
//! The schema is versioned. v1 (M3..M6.4) was a flat bag of scan/editor knobs
//! plus a single `notifications_enabled` bool. v2 (M6.5+) wraps notification
//! preferences in a struct so per-event toggles + Do-Not-Disturb fit, and
//! introduces `poll_interval_minutes` so users can dial the aggregator
//! cadence without rebuilding. v3 (M7) adds `terminal_command` for the
//! "Open in terminal" quick action. The migration is silent and one-shot: on
//! the first launch after upgrade, `load()` rewrites the file in v3 form.

use crate::util::atomic_write;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::{AppHandle, Manager};

/// Current schema version. Bumped on every breaking change to the on-disk
/// JSON layout. Migration logic in `migrate_from_value` covers v1→v2→v3; later
/// bumps should add a `vN_to_vN_plus_1` step rather than rewriting history.
pub const CURRENT_VERSION: u32 = 3;

/// Sane band for the user-configurable polling cadence (minutes).
/// Below 1 min hammers the provider APIs and burns rate-limit budget;
/// above 60 min loses the "menu-bar companion" feel. Clamped on load,
/// not validated in the UI, so a hand-edited file can never escape.
pub const POLL_INTERVAL_MIN: u32 = 1;
pub const POLL_INTERVAL_MAX: u32 = 60;
pub const POLL_INTERVAL_DEFAULT: u32 = 5;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// On-disk schema version. Always `CURRENT_VERSION` after `load()` —
    /// older files are migrated and rewritten before `load()` returns.
    #[serde(default = "default_version")]
    pub version: u32,
    /// Directories that the local scanner walks looking for `.git` checkouts.
    #[serde(default)]
    pub scan_roots: Vec<PathBuf>,
    /// Directory names that are never recursed into during the scan, on top
    /// of the always-skip list in `local_index::SKIP_DIRS`.
    #[serde(default)]
    pub scan_ignore: Vec<String>,
    /// Base URL for the connected GitLab instance (e.g. "https://gitlab.com"
    /// or "https://gitlab.gwdg.de"). The token itself lives in the Keychain.
    #[serde(default)]
    pub gitlab_base_url: Option<String>,
    /// Base URL for the connected Codeberg/Gitea/Forgejo instance. Defaults
    /// to https://codeberg.org when the user picks that radio in onboarding;
    /// can be overridden for self-hosted Gitea/Forgejo.
    #[serde(default)]
    pub codeberg_base_url: Option<String>,
    /// Shell command used by the "Open in editor" quick action. Whitespace-
    /// or empty-string disables the menu entry. The repo's local path is
    /// appended as the last argument (e.g. `"code"` becomes `code /Users/.../repo`).
    #[serde(default)]
    pub editor_command: Option<String>,
    /// macOS application name used by the "Open in terminal" quick action,
    /// launched via `open -a "<name>" <repo-path>` so a GUI terminal opens a
    /// new window in the repo directory (e.g. `"Terminal"`, `"iTerm"`,
    /// `"Warp"`). Whitespace- or empty-string disables the menu entry.
    #[serde(default)]
    pub terminal_command: Option<String>,
    /// Notification preferences. The frontend gates UI on these; the backend
    /// aggregator gates the actual `notifications::fire` call so a toggle
    /// flipped in one window takes effect everywhere via `settings-changed`.
    #[serde(default)]
    pub notifications: NotificationSettings,
    /// Aggregator polling cadence in minutes. Clamped to `[POLL_INTERVAL_MIN,
    /// POLL_INTERVAL_MAX]` on every load — a hand-edited or migrated value
    /// outside the band is silently corrected.
    #[serde(default = "default_poll_interval")]
    pub poll_interval_minutes: u32,
}

/// Top-level notification config. `enabled` is the master switch;
/// `do_not_disturb` is a temporary silence the user can toggle without
/// losing per-event preferences; `events` controls which categories fire
/// when the gates above let traffic through.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationSettings {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub do_not_disturb: bool,
    #[serde(default)]
    pub events: NotificationEventToggles,
}

/// Per-event silencing. All default to `true` so an upgraded install
/// notifies on everything until the user opts something out. `ci_failure`
/// is parsed and respected today but the diff that drives it lands in
/// Phase 3 with per-provider `author_login`; the toggle is harmless either
/// way.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationEventToggles {
    #[serde(default = "default_true")]
    pub waiting: bool,
    #[serde(default = "default_true")]
    pub releases: bool,
    #[serde(default = "default_true")]
    pub ci_failure: bool,
}

impl Default for NotificationSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            do_not_disturb: false,
            events: NotificationEventToggles::default(),
        }
    }
}

impl Default for NotificationEventToggles {
    fn default() -> Self {
        Self {
            waiting: true,
            releases: true,
            ci_failure: true,
        }
    }
}

fn default_true() -> bool {
    true
}
fn default_version() -> u32 {
    CURRENT_VERSION
}
fn default_poll_interval() -> u32 {
    POLL_INTERVAL_DEFAULT
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            scan_roots: default_scan_roots(),
            scan_ignore: Vec::new(),
            gitlab_base_url: None,
            codeberg_base_url: None,
            editor_command: None,
            terminal_command: None,
            notifications: NotificationSettings::default(),
            poll_interval_minutes: POLL_INTERVAL_DEFAULT,
        }
    }
}

impl Settings {
    /// Force every field into its sane band. Called after every load and
    /// before every save so a third-party edit can never poison the loop.
    fn clamp(&mut self) {
        self.poll_interval_minutes = self
            .poll_interval_minutes
            .clamp(POLL_INTERVAL_MIN, POLL_INTERVAL_MAX);
    }
}

/// First-launch scan roots — picks the first one of the common conventions
/// that actually exists on the user's home directory.
fn default_scan_roots() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    for candidate in ["Developer", "Code", "src", "Projects", "code"] {
        let p = home.join(candidate);
        if p.is_dir() {
            return vec![p];
        }
    }
    Vec::new()
}

fn config_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("resolving app config dir: {e}"))?;
    Ok(dir.join("settings.json"))
}

/// Read settings from disk, migrating older schemas in place. The first
/// `load()` after an upgrade rewrites the file so subsequent reads are a
/// straight `serde_json::from_str` of the canonical v2 form.
pub fn load(app: &AppHandle) -> Result<Settings, String> {
    let path = config_path(app)?;
    if !path.exists() {
        return Ok(Settings::default());
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("reading {path:?}: {e}"))?;
    let value: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("parsing {path:?}: {e}"))?;

    let on_disk_version = value.get("version").and_then(|v| v.as_u64()).unwrap_or(1) as u32;

    let needs_persist = on_disk_version < CURRENT_VERSION;

    let mut settings: Settings = if needs_persist {
        migrate_from_value(value, on_disk_version)?
    } else {
        serde_json::from_value(value).map_err(|e| format!("parsing {path:?}: {e}"))?
    };
    settings.clamp();

    if needs_persist {
        // Persist the migrated form so a later `load()` sees v2 directly
        // and so anything that pokes at the JSON externally (Spotlight
        // preview, support-bundle dump) sees the canonical shape.
        save(app, &settings)?;
    }

    Ok(settings)
}

pub fn save(app: &AppHandle, settings: &Settings) -> Result<(), String> {
    let mut to_write = settings.clone();
    to_write.clamp();
    to_write.version = CURRENT_VERSION;
    let path = config_path(app)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir -p {parent:?}: {e}"))?;
    }
    let json = serde_json::to_string_pretty(&to_write)
        .map_err(|e| format!("serialising settings: {e}"))?;
    atomic_write(&path, json.as_bytes())
}

/// Pure migration step — given a raw JSON value and the version it claims
/// to be, produce a `Settings` in the current schema. Kept free of
/// `AppHandle` so the migration is unit-testable.
fn migrate_from_value(value: serde_json::Value, from_version: u32) -> Result<Settings, String> {
    match from_version {
        // v1's notification shape differs from the current struct, so it gets
        // a dedicated step that also lands the v3 fields at their defaults.
        0 | 1 => migrate_v1_to_v2(value),
        2 => migrate_v2_to_v3(value),
        // A future bump would chain here: 3 → 4 → ... using the loaded
        // intermediate Settings as input.
        v => Err(format!("unsupported settings version on disk: {v}")),
    }
}

fn migrate_v1_to_v2(value: serde_json::Value) -> Result<Settings, String> {
    #[derive(Deserialize)]
    struct V1 {
        #[serde(default)]
        scan_roots: Vec<PathBuf>,
        #[serde(default)]
        scan_ignore: Vec<String>,
        #[serde(default)]
        gitlab_base_url: Option<String>,
        #[serde(default)]
        codeberg_base_url: Option<String>,
        #[serde(default)]
        editor_command: Option<String>,
        #[serde(default = "default_true")]
        notifications_enabled: bool,
    }
    let v1: V1 = serde_json::from_value(value).map_err(|e| format!("v1 parse: {e}"))?;
    Ok(Settings {
        version: CURRENT_VERSION,
        scan_roots: v1.scan_roots,
        scan_ignore: v1.scan_ignore,
        gitlab_base_url: v1.gitlab_base_url,
        codeberg_base_url: v1.codeberg_base_url,
        editor_command: v1.editor_command,
        // v3 field — v1 predates the "Open in terminal" action.
        terminal_command: None,
        notifications: NotificationSettings {
            // Carry the v1 master toggle forward; per-event toggles default
            // to "on" so the upgrade doesn't surprise the user with new
            // silences they didn't ask for.
            enabled: v1.notifications_enabled,
            ..Default::default()
        },
        poll_interval_minutes: POLL_INTERVAL_DEFAULT,
    })
}

/// v2→v3 added `terminal_command`. v2's on-disk shape is otherwise identical
/// to the current struct, and `terminal_command` carries `#[serde(default)]`,
/// so the current `Settings` deserialises a v2 file directly with the new
/// field defaulting to `None`. We only restamp the version.
fn migrate_v2_to_v3(value: serde_json::Value) -> Result<Settings, String> {
    let mut settings: Settings =
        serde_json::from_value(value).map_err(|e| format!("v2 parse: {e}"))?;
    settings.version = CURRENT_VERSION;
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn migrates_v1_with_notifications_enabled_true() {
        let v1 = json!({
            "scan_roots": ["/Users/x/Developer"],
            "scan_ignore": ["target"],
            "gitlab_base_url": "https://gitlab.com",
            "codeberg_base_url": null,
            "editor_command": "code",
            "notifications_enabled": true,
        });
        let s = migrate_from_value(v1, 1).expect("migration");
        assert_eq!(s.version, CURRENT_VERSION);
        assert!(s.notifications.enabled);
        assert!(!s.notifications.do_not_disturb);
        assert!(s.notifications.events.waiting);
        assert!(s.notifications.events.releases);
        assert!(s.notifications.events.ci_failure);
        assert_eq!(s.poll_interval_minutes, POLL_INTERVAL_DEFAULT);
        assert_eq!(s.editor_command.as_deref(), Some("code"));
        // v1 predates the terminal action — it lands at its default.
        assert_eq!(s.terminal_command, None);
    }

    #[test]
    fn migrates_v1_carries_notifications_enabled_false() {
        let v1 = json!({ "notifications_enabled": false });
        let s = migrate_from_value(v1, 1).expect("migration");
        assert!(!s.notifications.enabled);
        // Per-event defaults stay true — the master switch is the gate.
        assert!(s.notifications.events.waiting);
    }

    #[test]
    fn missing_version_treated_as_v1() {
        // The on-disk file from M3..M6.4 had no `version` key.
        let v1: serde_json::Value =
            serde_json::from_str(r#"{"scan_roots":[],"notifications_enabled":true}"#).unwrap();
        let on_disk_version = v1.get("version").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
        assert_eq!(on_disk_version, 1);
        let s = migrate_from_value(v1, on_disk_version).expect("migration");
        assert_eq!(s.version, CURRENT_VERSION);
    }

    #[test]
    fn migrates_v2_to_v3_defaults_terminal_command() {
        // A canonical v2 file: every current field except `terminal_command`,
        // which v2 predates. The migration restamps the version and leaves the
        // new field at its `None` default.
        let v2 = json!({
            "version": 2,
            "scan_roots": ["/Users/x/Developer"],
            "scan_ignore": ["target"],
            "gitlab_base_url": "https://gitlab.gwdg.de",
            "codeberg_base_url": null,
            "editor_command": "code",
            "notifications": {
                "enabled": true,
                "do_not_disturb": false,
                "events": { "waiting": true, "releases": false, "ci_failure": true }
            },
            "poll_interval_minutes": 10,
        });
        let s = migrate_from_value(v2, 2).expect("migration");
        assert_eq!(s.version, CURRENT_VERSION);
        assert_eq!(s.terminal_command, None);
        // Existing v2 fields survive untouched.
        assert_eq!(s.editor_command.as_deref(), Some("code"));
        assert_eq!(s.gitlab_base_url.as_deref(), Some("https://gitlab.gwdg.de"));
        assert_eq!(s.poll_interval_minutes, 10);
        assert!(!s.notifications.events.releases);
    }

    #[test]
    fn clamp_pins_poll_interval_into_band() {
        let mut s = Settings {
            poll_interval_minutes: 0,
            ..Settings::default()
        };
        s.clamp();
        assert_eq!(s.poll_interval_minutes, POLL_INTERVAL_MIN);

        let mut s = Settings {
            poll_interval_minutes: 9_999,
            ..Settings::default()
        };
        s.clamp();
        assert_eq!(s.poll_interval_minutes, POLL_INTERVAL_MAX);

        let mut s = Settings {
            poll_interval_minutes: 7,
            ..Settings::default()
        };
        s.clamp();
        assert_eq!(s.poll_interval_minutes, 7);
    }

    #[test]
    fn v2_roundtrips_via_json() {
        let s = Settings::default();
        let raw = serde_json::to_string(&s).unwrap();
        let parsed: Settings = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed.version, CURRENT_VERSION);
        assert_eq!(parsed.poll_interval_minutes, POLL_INTERVAL_DEFAULT);
        assert!(parsed.notifications.enabled);
    }
}
