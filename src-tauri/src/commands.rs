//! Tauri commands bridging the Svelte frontend to the providers.
//!
//! M2 added GitHub PAT auth + waiting items + repo list.
//! M3 layers on the local-index commands so the UI can surface "this repo
//! is cloned at ~/Developer/x" next to remote results.

use crate::{
    github::GitHubProvider,
    keychain, local_index,
    local_index::LocalRepo,
    settings::{self, Settings},
    types::*,
};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Account key used when storing a GitHub PAT in the Keychain. We commit to
/// "single default GitHub account" for M2; later milestones promote this to
/// something like `"github:work"` / `"github:personal"`.
const GH_ACCOUNT_KEY: &str = "github";

#[derive(Default)]
pub struct AppState {
    pub github: RwLock<Option<Arc<GitHubProvider>>>,
    /// Guards the one-time "have we tried to restore from the Keychain yet?"
    /// transition. Lazy-init protects against the popover's `onMount` calling
    /// `gh_status` before an eager startup task would have finished.
    init_attempted: tokio::sync::Mutex<bool>,
}

impl AppState {
    /// Restore a previously-saved GitHub token from the Keychain, exactly
    /// once per app lifetime. Subsequent calls return immediately. If the
    /// keychain entry is missing or its token has been revoked, the provider
    /// stays unset and the UI shows the onboarding form.
    pub async fn ensure_initialized(&self) {
        let mut attempted = self.init_attempted.lock().await;
        if *attempted {
            return;
        }
        *attempted = true;

        let token = match keychain::load(GH_ACCOUNT_KEY).await {
            Ok(Some(t)) => t,
            Ok(None) => return,
            Err(e) => {
                eprintln!("gitbuddy: keychain load failed: {e}");
                return;
            }
        };
        match GitHubProvider::connect(token).await {
            Ok(provider) => {
                *self.github.write().await = Some(Arc::new(provider));
            }
            Err(e) => {
                eprintln!("gitbuddy: restoring github session failed: {e}");
            }
        }
    }
}

/// Verify a GitHub PAT, store it in the Keychain, and make it the active
/// account. Returns the authenticated viewer for the UI to display.
#[tauri::command]
pub async fn gh_set_token(
    state: tauri::State<'_, Arc<AppState>>,
    token: String,
) -> Result<Viewer, String> {
    let provider = GitHubProvider::connect(token.clone())
        .await
        .map_err(|e| e.to_string())?;
    let viewer = provider.viewer.clone();

    keychain::save(GH_ACCOUNT_KEY, &token)
        .await
        .map_err(|e| format!("keychain: {e}"))?;

    *state.github.write().await = Some(Arc::new(provider));
    Ok(viewer)
}

/// Return the currently-connected GitHub viewer, or `None` if no account is
/// configured yet. Used by the frontend on load to decide between empty
/// state and live data. Triggers (and waits for) the one-time keychain
/// restoration on the first call after startup.
#[tauri::command]
pub async fn gh_status(state: tauri::State<'_, Arc<AppState>>) -> Result<Option<Viewer>, String> {
    state.ensure_initialized().await;
    let guard = state.github.read().await;
    Ok(guard.as_ref().map(|p| p.viewer.clone()))
}

#[tauri::command]
pub async fn gh_list_waiting(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<Vec<WaitingItem>, String> {
    let provider = state.github.read().await.clone();
    let Some(provider) = provider else {
        return Ok(Vec::new());
    };
    provider.list_waiting().await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn gh_list_repos(state: tauri::State<'_, Arc<AppState>>) -> Result<Vec<Repo>, String> {
    let provider = state.github.read().await.clone();
    let Some(provider) = provider else {
        return Ok(Vec::new());
    };
    provider.list_repos().await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn gh_list_releases(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<Vec<Release>, String> {
    let provider = state.github.read().await.clone();
    let Some(provider) = provider else {
        return Ok(Vec::new());
    };
    provider.list_releases().await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn gh_list_ci(state: tauri::State<'_, Arc<AppState>>) -> Result<Vec<CiRun>, String> {
    let provider = state.github.read().await.clone();
    let Some(provider) = provider else {
        return Ok(Vec::new());
    };
    provider.list_ci().await.map_err(|e| e.to_string())
}

// ── M3: local index ─────────────────────────────────────────────────────────

/// Walk the configured scan roots and return every Git checkout found, with
/// per-repo diagnostics. Scan runs on tokio's blocking pool so the async
/// runtime keeps responsive even on slow disks.
#[tauri::command]
pub async fn list_local_repos(app: tauri::AppHandle) -> Result<Vec<LocalRepo>, String> {
    let settings = settings::load(&app)?;
    tokio::task::spawn_blocking(move || local_index::scan(&settings))
        .await
        .map_err(|e| format!("scan task panicked: {e}"))
}

#[tauri::command]
pub fn get_settings(app: tauri::AppHandle) -> Result<Settings, String> {
    settings::load(&app)
}

#[tauri::command]
pub fn save_settings(app: tauri::AppHandle, settings: Settings) -> Result<(), String> {
    settings::save(&app, &settings)
}
