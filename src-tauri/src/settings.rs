//! Persistent user settings stored as JSON inside the OS-standard application
//! support directory (e.g. `~/Library/Application Support/dev.soron2038.gitbuddy/`).
//!
//! Kept deliberately tiny in M3 — only what's needed to drive the local
//! scanner. Notification preferences, polling cadence, theme overrides etc.
//! join later milestones.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// Directories that the local scanner walks looking for `.git` checkouts.
    pub scan_roots: Vec<PathBuf>,
    /// Directory names that are never recursed into during the scan, on top
    /// of the always-skip list in `local_index::SKIP_DIRS`.
    #[serde(default)]
    pub scan_ignore: Vec<String>,
    /// Base URL for the connected GitLab instance (e.g. "https://gitlab.com"
    /// or "https://gitlab.gwdg.de"). The token itself lives in the Keychain.
    #[serde(default)]
    pub gitlab_base_url: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            scan_roots: default_scan_roots(),
            scan_ignore: Vec::new(),
            gitlab_base_url: None,
        }
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

/// Read settings from disk, or return the defaults if no file exists yet.
pub fn load(app: &AppHandle) -> Result<Settings, String> {
    let path = config_path(app)?;
    if !path.exists() {
        return Ok(Settings::default());
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("reading {path:?}: {e}"))?;
    serde_json::from_str(&raw).map_err(|e| format!("parsing {path:?}: {e}"))
}

pub fn save(app: &AppHandle, settings: &Settings) -> Result<(), String> {
    let path = config_path(app)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir -p {parent:?}: {e}"))?;
    }
    let json =
        serde_json::to_string_pretty(settings).map_err(|e| format!("serialising settings: {e}"))?;
    atomic_write(&path, json.as_bytes())
}

/// Write `bytes` to `path` via a temp file + rename, so a crash mid-write
/// can't truncate the existing settings file.
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).map_err(|e| format!("writing {tmp:?}: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("renaming {tmp:?} → {path:?}: {e}"))?;
    Ok(())
}
