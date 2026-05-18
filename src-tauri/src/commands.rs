//! Tauri commands bridging the Svelte frontend to the providers.
//!
//! M2 added GitHub PAT auth + waiting items + repo list.
//! M3 layered on the local index for "this repo is also cloned at ~/x" joins.
//! M4 added releases, CI status, polling.
//! M5 generalises beyond GitHub: GitLab (gitlab.com + self-hosted) lives next
//! to GitHub, and the data-fetching commands aggregate across whichever
//! providers happen to be connected.

use crate::{
    accounts,
    codeberg::CodebergProvider,
    github::GitHubProvider,
    gitlab::GitLabProvider,
    keychain, local_index,
    local_index::LocalRepo,
    oauth::{self, DeviceCodeResponse, PollOutcome},
    settings::{self, Settings},
    types::*,
};
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::sync::RwLock;

/// Event name fired whenever a provider connects or disconnects. Both the
/// popover and the main window subscribe and re-fetch on receipt, so the
/// two stay consistent without per-window polling at the auth layer.
const EVT_PROVIDER_CHANGED: &str = "provider-changed";
/// Fired by `save_settings` so both windows pick up changes to the editor
/// command, notification toggle, scan roots, etc. without waiting for a
/// restart or the next 5-minute poll.
const EVT_SETTINGS_CHANGED: &str = "settings-changed";

// Pre-M6.3 single-account Keychain keys. The migration in
// `AppState::ensure_initialized` walks these once on first launch of the new
// build, copies each token under its composite per-account key
// (`<provider>:<login>`), records the account in `accounts.json`, and then
// deletes the legacy entry.
const GH_LEGACY_KEY: &str = "github";
const GL_LEGACY_KEY: &str = "gitlab";
const CB_LEGACY_KEY: &str = "codeberg";

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
    /// On the first command call after launch:
    ///   1. Migrate any pre-M6.3 legacy single-account Keychain entries to
    ///      per-account composite keys + record them in `accounts.json`.
    ///   2. Restore providers from `accounts.json`. Each account is restored
    ///      independently — a failure for one doesn't blank the rest.
    pub async fn ensure_initialized(&self, app: &AppHandle) {
        let mut attempted = self.init_attempted.lock().await;
        if *attempted {
            return;
        }
        *attempted = true;

        migrate_legacy_keychain(app).await;
        restore_from_accounts(app, self).await;
    }
}

/// Best-effort one-shot upgrade of the pre-M6.3 single-account Keychain
/// layout. For each legacy provider key that still exists and isn't yet
/// represented in `accounts.json`: connect with the legacy token, derive the
/// composite key `<provider>:<login>`, save the token under the new key,
/// upsert the account record, and delete the legacy key. If the legacy
/// token is revoked or the network is down, the legacy entry is left alone
/// so a later launch can retry — no destructive cleanup before the
/// migration confirms success.
async fn migrate_legacy_keychain(app: &AppHandle) {
    let existing = accounts::load(app).unwrap_or_default();
    let has = |slug: &str| existing.accounts.iter().any(|a| a.id.starts_with(slug));

    // GitHub
    if !has("github:") {
        match keychain::load(GH_LEGACY_KEY).await {
            Ok(Some(token)) => match GitHubProvider::connect(token.clone()).await {
                Ok(p) => {
                    let account =
                        accounts::account_from(Provider::Github, &p.viewer, AuthMethod::Pat, None);
                    finalise_migration(app, GH_LEGACY_KEY, account, &token).await;
                }
                Err(e) => eprintln!("gitbuddy: legacy github token invalid, leaving in place: {e}"),
            },
            Ok(None) => {}
            Err(e) => eprintln!("gitbuddy: keychain load (legacy github) failed: {e}"),
        }
    }

    // GitLab — needs the saved base URL.
    if !has("gitlab:") {
        let stored = settings::load(app).ok();
        let gl_base = stored.as_ref().and_then(|s| s.gitlab_base_url.clone());
        if let Some(base_url) = gl_base {
            match keychain::load(GL_LEGACY_KEY).await {
                Ok(Some(token)) => {
                    match GitLabProvider::connect(token.clone(), base_url.clone()).await {
                        Ok(p) => {
                            let account = accounts::account_from(
                                Provider::Gitlab,
                                &p.viewer,
                                AuthMethod::Pat,
                                Some(p.base_url().to_string()),
                            );
                            finalise_migration(app, GL_LEGACY_KEY, account, &token).await;
                        }
                        Err(e) => {
                            eprintln!(
                                "gitbuddy: legacy gitlab token invalid, leaving in place: {e}"
                            )
                        }
                    }
                }
                Ok(None) => {}
                Err(e) => eprintln!("gitbuddy: keychain load (legacy gitlab) failed: {e}"),
            }
        }
    }

    // Codeberg / Gitea / Forgejo — base URL stored alongside.
    if !has("codeberg:") {
        let stored = settings::load(app).ok();
        let cb_base = stored
            .as_ref()
            .and_then(|s| s.codeberg_base_url.clone())
            .unwrap_or_else(|| "https://codeberg.org".to_string());
        match keychain::load(CB_LEGACY_KEY).await {
            Ok(Some(token)) => match CodebergProvider::connect(token.clone(), cb_base).await {
                Ok(p) => {
                    let account = accounts::account_from(
                        Provider::Codeberg,
                        &p.viewer,
                        AuthMethod::Pat,
                        Some(p.base_url().to_string()),
                    );
                    finalise_migration(app, CB_LEGACY_KEY, account, &token).await;
                }
                Err(e) => {
                    eprintln!("gitbuddy: legacy codeberg token invalid, leaving in place: {e}")
                }
            },
            Ok(None) => {}
            Err(e) => eprintln!("gitbuddy: keychain load (legacy codeberg) failed: {e}"),
        }
    }
}

/// Persist the migrated account: write the token under the new composite
/// key, upsert the registry row, then delete the legacy entry. The legacy
/// delete is last so any failure earlier leaves the system in a state where
/// the next launch can retry from scratch.
async fn finalise_migration(
    app: &AppHandle,
    legacy_key: &str,
    account: crate::types::Account,
    token: &str,
) {
    if let Err(e) = keychain::save(&account.id, token).await {
        eprintln!(
            "gitbuddy: writing migrated token under {} failed: {e}",
            account.id
        );
        return;
    }
    if let Err(e) = accounts::upsert(app, account.clone()) {
        eprintln!(
            "gitbuddy: writing migrated account record for {} failed: {e}",
            account.id
        );
        return;
    }
    if let Err(e) = keychain::delete(legacy_key).await {
        eprintln!("gitbuddy: deleting legacy keychain key {legacy_key} failed: {e}");
    }
}

/// Restore providers from `accounts.json` into the in-memory `AppState`.
/// Currently single-account-per-provider in the UI, so we just pick the
/// first record per provider; multi-account UI later will iterate.
async fn restore_from_accounts(app: &AppHandle, state: &AppState) {
    let file = match accounts::load(app) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("gitbuddy: reading accounts.json failed: {e}");
            return;
        }
    };

    for account in file.accounts {
        let raw = match keychain::load(&account.id).await {
            Ok(Some(t)) => t,
            Ok(None) => {
                eprintln!(
                    "gitbuddy: keychain entry for {} missing — orphan account record",
                    account.id
                );
                continue;
            }
            Err(e) => {
                eprintln!("gitbuddy: keychain load ({}) failed: {e}", account.id);
                continue;
            }
        };

        // PAT entries are bare token strings; OAuth entries are a JSON blob
        // wrapping the access_token plus its scope and obtained_at. The
        // providers all want a bare bearer token, so unpack here.
        let token = match account.auth {
            AuthMethod::Pat => raw,
            AuthMethod::OauthDevice => {
                match serde_json::from_str::<crate::oauth::OAuthTokens>(&raw) {
                    Ok(t) => t.access_token,
                    Err(e) => {
                        eprintln!(
                            "gitbuddy: oauth tokens blob for {} unparseable: {e}",
                            account.id
                        );
                        continue;
                    }
                }
            }
        };

        match account.provider {
            Provider::Github => match GitHubProvider::connect(token).await {
                Ok(p) => *state.github.write().await = Some(Arc::new(p)),
                Err(e) => eprintln!("gitbuddy: restoring github session failed: {e}"),
            },
            Provider::Gitlab | Provider::MpsdGitlab => {
                let Some(base_url) = account.base_url.clone() else {
                    eprintln!(
                        "gitbuddy: gitlab account {} missing base_url, skipping",
                        account.id
                    );
                    continue;
                };
                match GitLabProvider::connect(token, base_url).await {
                    Ok(p) => *state.gitlab.write().await = Some(Arc::new(p)),
                    Err(e) => eprintln!("gitbuddy: restoring gitlab session failed: {e}"),
                }
            }
            Provider::Codeberg => {
                let base_url = account
                    .base_url
                    .clone()
                    .unwrap_or_else(|| "https://codeberg.org".to_string());
                match CodebergProvider::connect(token, base_url).await {
                    Ok(p) => *state.codeberg.write().await = Some(Arc::new(p)),
                    Err(e) => eprintln!("gitbuddy: restoring codeberg session failed: {e}"),
                }
            }
        }
    }
}

/// Look up the composite-key id of the currently-connected account for a
/// given provider. The in-memory provider state is the freshest source;
/// `accounts.json` is the persistent fallback for disconnect calls that
/// race against an in-memory clear. Returns `None` when the provider isn't
/// connected at all (and the caller should fall through to legacy-key
/// cleanup as a belt-and-braces).
async fn current_account_id(
    state: tauri::State<'_, Arc<AppState>>,
    provider: Provider,
    app: &AppHandle,
) -> Option<String> {
    let in_memory_login = match provider {
        Provider::Github => state
            .github
            .read()
            .await
            .as_ref()
            .map(|p| p.viewer.login.clone()),
        Provider::Gitlab | Provider::MpsdGitlab => state
            .gitlab
            .read()
            .await
            .as_ref()
            .map(|p| p.viewer.login.clone()),
        Provider::Codeberg => state
            .codeberg
            .read()
            .await
            .as_ref()
            .map(|p| p.viewer.login.clone()),
    };
    if let Some(login) = in_memory_login {
        return Some(accounts::make_id(provider, &login));
    }

    accounts::load(app)
        .ok()?
        .accounts
        .into_iter()
        .find(|a| a.provider == provider)
        .map(|a| a.id)
}

// ── Per-provider auth commands ─────────────────────────────────────────────

#[tauri::command]
pub async fn gh_set_token(
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
    token: String,
) -> Result<Viewer, String> {
    let provider = GitHubProvider::connect(token.clone())
        .await
        .map_err(|e| e.to_string())?;
    let viewer = provider.viewer.clone();
    let account = accounts::account_from(Provider::Github, &viewer, AuthMethod::Pat, None);
    keychain::save(&account.id, &token)
        .await
        .map_err(|e| format!("keychain: {e}"))?;
    accounts::upsert(&app, account)?;
    *state.github.write().await = Some(Arc::new(provider));
    let _ = app.emit(EVT_PROVIDER_CHANGED, ());
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

/// Kick off the GitHub OAuth Device Flow. Returns the user_code (for the
/// human to enter at `verification_uri`) plus the device_code + poll interval
/// the caller must echo back into `gh_oauth_poll`. The browser is opened by
/// the frontend via `tauri-plugin-opener`, consistent with how the existing
/// "Create a token" links work.
#[tauri::command]
pub async fn gh_oauth_begin() -> Result<DeviceCodeResponse, String> {
    let client = oauth_http_client()?;
    oauth::begin_github(&client)
        .await
        .map_err(|e| e.to_string())
}

/// Outcome of a single Device Flow poll, returned to the frontend so it can
/// either keep polling (Pending / SlowDown), surface an error (Denied /
/// Expired), or transition to the connected state (Success). The discriminant
/// tag is `kind` to match `oauth::PollOutcome`, with the connected viewer
/// merged in for the Success arm so the UI can show "Connected as @login"
/// without a follow-up round-trip.
#[derive(serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GhOAuthPollResult {
    Success { viewer: Viewer },
    Pending,
    SlowDown { interval: u64 },
    Denied,
    Expired,
}

#[tauri::command]
pub async fn gh_oauth_poll(
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
    device_code: String,
) -> Result<GhOAuthPollResult, String> {
    let client = oauth_http_client()?;
    let outcome = oauth::poll_github(&client, &device_code)
        .await
        .map_err(|e| e.to_string())?;

    match outcome {
        PollOutcome::Pending => Ok(GhOAuthPollResult::Pending),
        PollOutcome::SlowDown { interval } => Ok(GhOAuthPollResult::SlowDown { interval }),
        PollOutcome::Denied => Ok(GhOAuthPollResult::Denied),
        PollOutcome::Expired => Ok(GhOAuthPollResult::Expired),
        PollOutcome::Success(tokens) => {
            // Validate the token works against /user and populate the viewer.
            // If GitHub immediately rejects it, surface as an error so the
            // UI can ask the user to retry rather than pretending the
            // connection succeeded.
            let provider = GitHubProvider::connect(tokens.access_token.clone())
                .await
                .map_err(|e| format!("validating OAuth token: {e}"))?;
            let viewer = provider.viewer.clone();
            let account =
                accounts::account_from(Provider::Github, &viewer, AuthMethod::OauthDevice, None);
            // Persist the full tokens blob, not just the access_token —
            // future fields (refresh_token if an org enables expiration,
            // obtained_at for staleness checks) live there.
            let blob = serde_json::to_string(&tokens)
                .map_err(|e| format!("serialising oauth tokens: {e}"))?;
            keychain::save(&account.id, &blob)
                .await
                .map_err(|e| format!("keychain: {e}"))?;
            accounts::upsert(&app, account)?;
            *state.github.write().await = Some(Arc::new(provider));
            let _ = app.emit(EVT_PROVIDER_CHANGED, ());
            Ok(GhOAuthPollResult::Success { viewer })
        }
    }
}

fn oauth_http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(concat!("gitBuddy/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("http client: {e}"))
}

#[tauri::command]
pub async fn gh_disconnect(
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
) -> Result<(), String> {
    // Resolve the composite key from whatever source we have. Prefer the
    // in-memory provider (always current); fall back to accounts.json in
    // case the in-memory state was cleared but the registry record lingers;
    // last-ditch the legacy key for any install that's somehow still in
    // pre-migration shape.
    if let Some(id) = current_account_id(state.clone(), Provider::Github, &app).await {
        let _ = keychain::delete(&id).await;
        let _ = accounts::remove(&app, &id);
    } else {
        let _ = keychain::delete(GH_LEGACY_KEY).await;
    }
    *state.github.write().await = None;
    let _ = app.emit(EVT_PROVIDER_CHANGED, ());
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
    let account = accounts::account_from(
        Provider::Gitlab,
        &viewer,
        AuthMethod::Pat,
        Some(provider.base_url().to_string()),
    );
    keychain::save(&account.id, &token)
        .await
        .map_err(|e| format!("keychain: {e}"))?;
    accounts::upsert(&app, account)?;
    // Keep gitlab_base_url in settings up to date — `ensure_initialized` no
    // longer reads it, but it's still consumed by the onboarding modal to
    // pre-fill the host suggestion next time the user reconnects.
    let mut s = settings::load(&app).unwrap_or_default();
    s.gitlab_base_url = Some(provider.base_url().to_string());
    settings::save(&app, &s)?;

    *state.gitlab.write().await = Some(Arc::new(provider));
    let _ = app.emit(EVT_PROVIDER_CHANGED, ());
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
    if let Some(id) = current_account_id(state.clone(), Provider::Gitlab, &app).await {
        let _ = keychain::delete(&id).await;
        let _ = accounts::remove(&app, &id);
    } else {
        let _ = keychain::delete(GL_LEGACY_KEY).await;
    }
    *state.gitlab.write().await = None;
    // Clear the base URL too so the next `+ Add` flow starts from
    // gitlab.com rather than re-suggesting the disconnected host.
    let mut s = settings::load(&app).unwrap_or_default();
    s.gitlab_base_url = None;
    settings::save(&app, &s)?;
    let _ = app.emit(EVT_PROVIDER_CHANGED, ());
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
    let account = accounts::account_from(
        Provider::Codeberg,
        &viewer,
        AuthMethod::Pat,
        Some(provider.base_url().to_string()),
    );
    keychain::save(&account.id, &token)
        .await
        .map_err(|e| format!("keychain: {e}"))?;
    accounts::upsert(&app, account)?;
    let mut s = settings::load(&app).unwrap_or_default();
    s.codeberg_base_url = Some(provider.base_url().to_string());
    settings::save(&app, &s)?;

    *state.codeberg.write().await = Some(Arc::new(provider));
    let _ = app.emit(EVT_PROVIDER_CHANGED, ());
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
    if let Some(id) = current_account_id(state.clone(), Provider::Codeberg, &app).await {
        let _ = keychain::delete(&id).await;
        let _ = accounts::remove(&app, &id);
    } else {
        let _ = keychain::delete(CB_LEGACY_KEY).await;
    }
    *state.codeberg.write().await = None;
    let mut s = settings::load(&app).unwrap_or_default();
    s.codeberg_base_url = None;
    settings::save(&app, &s)?;
    let _ = app.emit(EVT_PROVIDER_CHANGED, ());
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

/// Same as `open_main` but also tells the main window to switch to its
/// Settings view. The popover wires its gear icon here so settings live in
/// the spacious main window rather than inside the 360 px popover.
#[tauri::command]
pub fn open_main_settings(app: AppHandle) -> Result<(), String> {
    open_main(app.clone())?;
    let _ = app.emit("main-window-navigate", "settings");
    Ok(())
}

/// List all connected accounts.
///
/// Reads `accounts.json` first; if it's empty (pre-M6.3 installs that
/// haven't been migrated yet, or fresh installs that just connected via the
/// legacy single-account commands), synthesises records from whichever
/// providers are currently restored in memory. Once the migration in the
/// next commit runs on first launch, the synthesised path becomes a no-op
/// and `accounts.json` is the sole source of truth.
#[tauri::command]
pub async fn accounts_list(
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
) -> Result<Vec<Account>, String> {
    state.ensure_initialized(&app).await;

    let stored = accounts::load(&app)?;
    if !stored.accounts.is_empty() {
        return Ok(stored.accounts);
    }

    let mut out = Vec::new();
    if let Some(p) = state.github.read().await.as_ref() {
        out.push(accounts::account_from(
            Provider::Github,
            &p.viewer,
            AuthMethod::Pat,
            None,
        ));
    }
    if let Some(p) = state.gitlab.read().await.as_ref() {
        out.push(accounts::account_from(
            Provider::Gitlab,
            &p.viewer,
            AuthMethod::Pat,
            Some(p.base_url().to_string()),
        ));
    }
    if let Some(p) = state.codeberg.read().await.as_ref() {
        out.push(accounts::account_from(
            Provider::Codeberg,
            &p.viewer,
            AuthMethod::Pat,
            Some(p.base_url().to_string()),
        ));
    }
    Ok(out)
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
    settings::save(&app, &settings)?;
    let _ = app.emit(EVT_SETTINGS_CHANGED, ());
    Ok(())
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
