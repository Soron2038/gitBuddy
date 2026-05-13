//! Tauri commands bridging the Svelte frontend to the providers.
//!
//! M2 added GitHub PAT auth + waiting items + repo list.
//! M3 layered on the local index for "this repo is also cloned at ~/x" joins.
//! M4 added releases, CI status, polling.
//! M5 generalises beyond GitHub: GitLab (gitlab.com + self-hosted) lives next
//! to GitHub, and the data-fetching commands aggregate across whichever
//! providers happen to be connected.

use crate::{
    codeberg::CodebergProvider,
    github::GitHubProvider,
    gitlab::GitLabProvider,
    keychain, local_index,
    local_index::LocalRepo,
    settings::{self, Settings},
    types::*,
};
use std::sync::Arc;
use tauri::AppHandle;
use tokio::sync::RwLock;

const GH_KEYCHAIN_KEY: &str = "github";
const GL_KEYCHAIN_KEY: &str = "gitlab";
const CB_KEYCHAIN_KEY: &str = "codeberg";

#[derive(Default)]
pub struct AppState {
    pub github: RwLock<Option<Arc<GitHubProvider>>>,
    pub gitlab: RwLock<Option<Arc<GitLabProvider>>>,
    pub codeberg: RwLock<Option<Arc<CodebergProvider>>>,
    /// Gates the one-time keychain restore so commands can wait for the
    /// initial auth attempt before reporting "no providers connected".
    init_attempted: tokio::sync::Mutex<bool>,
}

impl AppState {
    /// Restore any tokens saved to the Keychain on first command call. Each
    /// provider is independent: GitHub may restore while GitLab doesn't,
    /// or vice versa.
    pub async fn ensure_initialized(&self, app: &AppHandle) {
        let mut attempted = self.init_attempted.lock().await;
        if *attempted {
            return;
        }
        *attempted = true;

        // GitHub — no per-account config beyond the token.
        match keychain::load(GH_KEYCHAIN_KEY).await {
            Ok(Some(token)) => match GitHubProvider::connect(token).await {
                Ok(p) => *self.github.write().await = Some(Arc::new(p)),
                Err(e) => eprintln!("gitbuddy: restoring github session failed: {e}"),
            },
            Ok(None) => {}
            Err(e) => eprintln!("gitbuddy: keychain load (github) failed: {e}"),
        }

        // GitLab — needs the saved base URL too.
        let stored = settings::load(app).ok();
        let gl_base = stored.as_ref().and_then(|s| s.gitlab_base_url.clone());
        if let Some(base_url) = gl_base {
            match keychain::load(GL_KEYCHAIN_KEY).await {
                Ok(Some(token)) => match GitLabProvider::connect(token, base_url).await {
                    Ok(p) => *self.gitlab.write().await = Some(Arc::new(p)),
                    Err(e) => eprintln!("gitbuddy: restoring gitlab session failed: {e}"),
                },
                Ok(None) => {}
                Err(e) => eprintln!("gitbuddy: keychain load (gitlab) failed: {e}"),
            }
        }

        // Codeberg / Gitea / Forgejo — base URL stored alongside.
        let cb_base = stored
            .as_ref()
            .and_then(|s| s.codeberg_base_url.clone())
            .unwrap_or_else(|| "https://codeberg.org".to_string());
        match keychain::load(CB_KEYCHAIN_KEY).await {
            Ok(Some(token)) => match CodebergProvider::connect(token, cb_base).await {
                Ok(p) => *self.codeberg.write().await = Some(Arc::new(p)),
                Err(e) => eprintln!("gitbuddy: restoring codeberg session failed: {e}"),
            },
            Ok(None) => {}
            Err(e) => eprintln!("gitbuddy: keychain load (codeberg) failed: {e}"),
        }
    }
}

// ── Per-provider auth commands ─────────────────────────────────────────────

#[tauri::command]
pub async fn gh_set_token(
    state: tauri::State<'_, Arc<AppState>>,
    token: String,
) -> Result<Viewer, String> {
    let provider = GitHubProvider::connect(token.clone())
        .await
        .map_err(|e| e.to_string())?;
    let viewer = provider.viewer.clone();
    keychain::save(GH_KEYCHAIN_KEY, &token)
        .await
        .map_err(|e| format!("keychain: {e}"))?;
    *state.github.write().await = Some(Arc::new(provider));
    Ok(viewer)
}

#[tauri::command]
pub async fn gh_status(
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
) -> Result<Option<Viewer>, String> {
    state.ensure_initialized(&app).await;
    Ok(state.github.read().await.as_ref().map(|p| p.viewer.clone()))
}

#[tauri::command]
pub async fn gh_disconnect(state: tauri::State<'_, Arc<AppState>>) -> Result<(), String> {
    // Delete the keychain entry before clearing in-memory state so a crash
    // mid-disconnect doesn't leave a stale token that would re-auth on
    // next launch.
    let _ = keychain::delete(GH_KEYCHAIN_KEY).await;
    *state.github.write().await = None;
    Ok(())
}

#[tauri::command]
pub async fn gl_set_token(
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
    token: String,
    base_url: String,
) -> Result<Viewer, String> {
    let provider = GitLabProvider::connect(token.clone(), base_url.clone())
        .await
        .map_err(|e| e.to_string())?;
    let viewer = provider.viewer.clone();
    // Persist both pieces: the token lives in the Keychain, the base URL in
    // the JSON settings (it's not secret and we need it before the keychain
    // load to know which host to talk to).
    keychain::save(GL_KEYCHAIN_KEY, &token)
        .await
        .map_err(|e| format!("keychain: {e}"))?;
    let mut s = settings::load(&app).unwrap_or_default();
    s.gitlab_base_url = Some(provider.base_url().to_string());
    settings::save(&app, &s)?;

    *state.gitlab.write().await = Some(Arc::new(provider));
    Ok(viewer)
}

#[tauri::command]
pub async fn gl_status(
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
) -> Result<Option<GitLabStatus>, String> {
    state.ensure_initialized(&app).await;
    Ok(state.gitlab.read().await.as_ref().map(|p| GitLabStatus {
        viewer: p.viewer.clone(),
        base_url: p.base_url().to_string(),
    }))
}

/// Returned by `gl_status`. The base URL is useful in the UI so we can show
/// "connected to gitlab.gwdg.de" without an extra command call.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct GitLabStatus {
    pub viewer: Viewer,
    pub base_url: String,
}

#[tauri::command]
pub async fn gl_disconnect(
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
) -> Result<(), String> {
    let _ = keychain::delete(GL_KEYCHAIN_KEY).await;
    *state.gitlab.write().await = None;
    // Clear the base URL too so the next `+ Add` flow starts from
    // gitlab.com rather than re-suggesting the disconnected host.
    let mut s = settings::load(&app).unwrap_or_default();
    s.gitlab_base_url = None;
    settings::save(&app, &s)?;
    Ok(())
}

#[tauri::command]
pub async fn cb_set_token(
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
    token: String,
    base_url: String,
) -> Result<Viewer, String> {
    let provider = CodebergProvider::connect(token.clone(), base_url.clone())
        .await
        .map_err(|e| e.to_string())?;
    let viewer = provider.viewer.clone();
    keychain::save(CB_KEYCHAIN_KEY, &token)
        .await
        .map_err(|e| format!("keychain: {e}"))?;
    let mut s = settings::load(&app).unwrap_or_default();
    s.codeberg_base_url = Some(provider.base_url().to_string());
    settings::save(&app, &s)?;

    *state.codeberg.write().await = Some(Arc::new(provider));
    Ok(viewer)
}

#[tauri::command]
pub async fn cb_status(
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
) -> Result<Option<CodebergStatus>, String> {
    state.ensure_initialized(&app).await;
    Ok(state
        .codeberg
        .read()
        .await
        .as_ref()
        .map(|p| CodebergStatus {
            viewer: p.viewer.clone(),
            base_url: p.base_url().to_string(),
        }))
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct CodebergStatus {
    pub viewer: Viewer,
    pub base_url: String,
}

#[tauri::command]
pub async fn cb_disconnect(
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
) -> Result<(), String> {
    let _ = keychain::delete(CB_KEYCHAIN_KEY).await;
    *state.codeberg.write().await = None;
    let mut s = settings::load(&app).unwrap_or_default();
    s.codeberg_base_url = None;
    settings::save(&app, &s)?;
    Ok(())
}

/// Reveal the main window and switch the app's activation policy to Regular
/// so it can take focus normally. Mirrors the tray menu's "Open gitBuddy"
/// item — exposed as a command so the popover can offer the same action.
#[tauri::command]
pub fn open_main(app: AppHandle) -> Result<(), String> {
    use tauri::Manager;
    let Some(window) = app.get_webview_window("main") else {
        return Err("main window not found".into());
    };
    #[cfg(target_os = "macos")]
    {
        let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
    }
    window.show().map_err(|e| e.to_string())?;
    let _ = window.unminimize();
    window.set_focus().map_err(|e| e.to_string())?;
    Ok(())
}

// ── Aggregated data commands ───────────────────────────────────────────────
//
// These fan out across every connected provider. A failure in one provider
// doesn't blank the whole result — its error is logged and the other
// providers' data is still returned. The popover never sees half a list.

#[tauri::command]
pub async fn list_waiting(
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
) -> Result<Vec<WaitingItem>, String> {
    state.ensure_initialized(&app).await;
    let gh = state.github.read().await.clone();
    let gl = state.gitlab.read().await.clone();
    let cb = state.codeberg.read().await.clone();

    let mut out = Vec::new();
    if let Some(p) = gh {
        match p.list_waiting().await {
            Ok(mut v) => out.append(&mut v),
            Err(e) => eprintln!("gitbuddy: github list_waiting failed: {e}"),
        }
    }
    if let Some(p) = gl {
        match p.list_waiting().await {
            Ok(mut v) => out.append(&mut v),
            Err(e) => eprintln!("gitbuddy: gitlab list_waiting failed: {e}"),
        }
    }
    if let Some(p) = cb {
        match p.list_waiting().await {
            Ok(mut v) => out.append(&mut v),
            Err(e) => eprintln!("gitbuddy: codeberg list_waiting failed: {e}"),
        }
    }
    out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(out)
}

#[tauri::command]
pub async fn list_repos(
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
) -> Result<Vec<Repo>, String> {
    state.ensure_initialized(&app).await;
    let gh = state.github.read().await.clone();
    let gl = state.gitlab.read().await.clone();
    let cb = state.codeberg.read().await.clone();

    let mut out = Vec::new();
    if let Some(p) = gh {
        match p.list_repos().await {
            Ok(mut v) => out.append(&mut v),
            Err(e) => eprintln!("gitbuddy: github list_repos failed: {e}"),
        }
    }
    if let Some(p) = gl {
        match p.list_repos().await {
            Ok(mut v) => out.append(&mut v),
            Err(e) => eprintln!("gitbuddy: gitlab list_repos failed: {e}"),
        }
    }
    if let Some(p) = cb {
        match p.list_repos().await {
            Ok(mut v) => out.append(&mut v),
            Err(e) => eprintln!("gitbuddy: codeberg list_repos failed: {e}"),
        }
    }
    out.sort_by(|a, b| b.pushed_at.cmp(&a.pushed_at));
    Ok(out)
}

#[tauri::command]
pub async fn list_releases(
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
) -> Result<Vec<Release>, String> {
    state.ensure_initialized(&app).await;
    // GitLab release listing isn't implemented yet (needs per-project release
    // fetches, gated to "recently active" projects to stay within rate limits).
    // For now, only GitHub contributes releases.
    let gh = state.github.read().await.clone();
    let Some(p) = gh else {
        return Ok(Vec::new());
    };
    p.list_releases().await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn list_ci(
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
) -> Result<Vec<CiRun>, String> {
    state.ensure_initialized(&app).await;
    // Same as releases: GitLab CI surface is a separate landing.
    let gh = state.github.read().await.clone();
    let Some(p) = gh else {
        return Ok(Vec::new());
    };
    p.list_ci().await.map_err(|e| e.to_string())
}

// ── Local index ─────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn list_local_repos(app: AppHandle) -> Result<Vec<LocalRepo>, String> {
    let settings = settings::load(&app)?;
    tokio::task::spawn_blocking(move || local_index::scan(&settings))
        .await
        .map_err(|e| format!("scan task panicked: {e}"))
}

#[tauri::command]
pub fn get_settings(app: AppHandle) -> Result<Settings, String> {
    settings::load(&app)
}

#[tauri::command]
pub fn save_settings(app: AppHandle, settings: Settings) -> Result<(), String> {
    settings::save(&app, &settings)
}

/// Run the user-configured editor command with `path` appended as the final
/// argument. Shells out via `sh -c` so PATH lookup (and aliases like `code`,
/// `cursor`, `zed`) work without us having to teach the binary about every
/// editor's install location.
#[tauri::command]
pub async fn run_editor(app: AppHandle, path: String) -> Result<(), String> {
    let settings = settings::load(&app)?;
    let cmd = settings.editor_command.unwrap_or_default();
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return Err("No editor command configured. Set one in Settings.".into());
    }

    // Single-arg shell escape: wrap path in single quotes, escape any
    // literal single quotes inside. Good enough for filesystem paths,
    // which can't contain newlines on macOS in normal use.
    let escaped_path = format!("'{}'", path.replace('\'', "'\\''"));
    let full = format!("{cmd} {escaped_path}");

    tokio::task::spawn_blocking(move || {
        std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(&full)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("spawning editor failed: {e}"))
    })
    .await
    .map_err(|e| format!("editor task panicked: {e}"))?
}
