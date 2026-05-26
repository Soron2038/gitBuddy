//! Tauri commands bridging the Svelte frontend to the providers.
//!
//! M2 added GitHub PAT auth + waiting items + repo list.
//! M3 layered on the local index for "this repo is also cloned at ~/x" joins.
//! M4 added releases, CI status, polling.
//! M5 generalises beyond GitHub: GitLab (gitlab.com + self-hosted) lives next
//! to GitHub, and the data-fetching commands aggregate across whichever
//! providers happen to be connected.

use crate::{
    accounts, aggregator,
    aggregator::AggregatorCache,
    codeberg::CodebergProvider,
    github::GitHubProvider,
    gitlab::GitLabProvider,
    keychain,
    local_index::LocalRepo,
    oauth::{self, DeviceCodeResponse, PollOutcome},
    settings::{self, Settings},
    types::*,
};
use std::collections::HashMap;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::sync::{Notify, RwLock};

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

/// In-memory provider registry. One HashMap per provider type, keyed by
/// `Account.id` (the v2 `<provider>:<host>:<login>` string). A single user
/// can hold multiple accounts per provider — e.g. two GitLab instances or a
/// personal + work GitHub — and each is restored / refreshed / removed
/// independently.
///
/// The `cache` + two `Notify`s drive the backend aggregator loop. Provider
/// commands here `refresh_trigger.notify_one()` after a connect/disconnect
/// so the cache repopulates immediately; `save_settings` notifies
/// `settings_reload` so the loop picks up a changed poll interval without
/// waiting out the current sleep.
pub struct AppState {
    pub github: RwLock<HashMap<String, Arc<GitHubProvider>>>,
    pub gitlab: RwLock<HashMap<String, Arc<GitLabProvider>>>,
    pub codeberg: RwLock<HashMap<String, Arc<CodebergProvider>>>,
    /// Snapshot of every aggregated list as of the last successful tick.
    /// `list_*` commands read from here instead of fanning out per call.
    pub cache: RwLock<AggregatorCache>,
    /// Notified by `aggregator_refresh_now` and every auth command. The
    /// aggregator loop waits on this alongside its periodic sleep so a
    /// trigger interrupts the sleep and runs an immediate tick.
    pub refresh_trigger: Arc<Notify>,
    /// Notified by `save_settings`. Same race as `refresh_trigger`: lets the
    /// loop re-read the poll interval mid-sleep.
    pub settings_reload: Arc<Notify>,
    /// Gates the one-time keychain restore so commands can wait for the
    /// initial auth attempt before reporting "no providers connected".
    init_attempted: tokio::sync::Mutex<bool>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            github: RwLock::new(HashMap::new()),
            gitlab: RwLock::new(HashMap::new()),
            codeberg: RwLock::new(HashMap::new()),
            cache: RwLock::new(AggregatorCache::default()),
            refresh_trigger: Arc::new(Notify::new()),
            settings_reload: Arc::new(Notify::new()),
            init_attempted: tokio::sync::Mutex::new(false),
        }
    }
}

impl AppState {
    /// On the first command call after launch:
    ///   1. Migrate `accounts.json` from v1 ids (`<provider>:<login>`, M6.3)
    ///      to v2 ids (`<provider>:<host>:<login>`, M6.4), moving each
    ///      Keychain entry to its new key.
    ///   2. Migrate any pre-M6.3 legacy flat Keychain entries
    ///      (`"github"` / `"gitlab"` / `"codeberg"`) directly into v2-format
    ///      account records.
    ///   3. Restore providers from `accounts.json`. Each account is restored
    ///      independently — a failure for one doesn't blank the rest.
    pub async fn ensure_initialized(&self, app: &AppHandle) {
        let mut attempted = self.init_attempted.lock().await;
        if *attempted {
            return;
        }
        *attempted = true;

        migrate_id_scheme_to_v2(app).await;
        migrate_legacy_keychain(app).await;
        restore_from_accounts(app, self).await;
    }
}

/// One-shot upgrade from v1 (`<provider>:<login>`) to v2
/// (`<provider>:<host>:<login>`) account ids. Walks every record in
/// `accounts.json`; for any whose computed v2 id differs from its stored id,
/// the Keychain entry is copied under the new id and the old entry deleted.
/// Records whose Keychain entries can't be read are left with the old id in
/// the registry so a later launch can retry — failing-open here would
/// destroy state. After the walk, `accounts.json` is bumped to
/// `CURRENT_VERSION` and saved.
async fn migrate_id_scheme_to_v2(app: &AppHandle) {
    let mut file = match accounts::load(app) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("gitbuddy: load accounts.json for v2 migration failed: {e}");
            return;
        }
    };
    if file.version >= accounts::CURRENT_VERSION {
        return;
    }

    let mut migrated = Vec::with_capacity(file.accounts.len());
    // Bump `accounts.json`'s version only if every account either was already
    // on v2 or got cleanly upgraded. Leaving the version at v1 on partial
    // failure lets the next launch retry; otherwise the early-return at the
    // top of this function would skip migration forever and the failed
    // accounts would be stuck on v1 ids despite the file claiming v2.
    let mut all_clean = true;
    for mut account in file.accounts {
        let host = accounts::account_host(account.provider, account.base_url.as_deref());
        let new_id = accounts::make_id(account.provider, &host, &account.login);
        if new_id == account.id {
            migrated.push(account);
            continue;
        }

        match keychain::load(&account.id).await {
            Ok(Some(secret)) => {
                if let Err(e) = keychain::save(&new_id, &secret).await {
                    eprintln!("gitbuddy: writing v2 keychain entry under {new_id} failed: {e}");
                    migrated.push(account);
                    all_clean = false;
                    continue;
                }
                if let Err(e) = keychain::delete(&account.id).await {
                    eprintln!(
                        "gitbuddy: deleting v1 keychain entry {} failed: {e} — leftover key is harmless",
                        account.id
                    );
                }
                account.id = new_id;
                migrated.push(account);
            }
            Ok(None) => {
                // Record without a Keychain entry: bumping or not bumping the
                // version doesn't change anything for it — the record is
                // orphaned regardless. Don't block the version bump on it.
                eprintln!(
                    "gitbuddy: v1 account {} has no keychain entry, leaving orphan record",
                    account.id
                );
                migrated.push(account);
            }
            Err(e) => {
                eprintln!(
                    "gitbuddy: keychain load for v2 migration of {} failed: {e}",
                    account.id
                );
                migrated.push(account);
                all_clean = false;
            }
        }
    }

    file.accounts = migrated;
    if all_clean {
        file.version = accounts::CURRENT_VERSION;
    }
    if let Err(e) = accounts::save(app, &file) {
        eprintln!("gitbuddy: writing v2 accounts.json failed: {e}");
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
/// Every account is restored — keyed by its id — so two GitLab instances or
/// a personal-plus-work GitHub end up co-resident in their respective
/// HashMaps. Each connect failure is logged but doesn't blank the rest.
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

        let id = account.id.clone();
        match account.provider {
            Provider::Github => match GitHubProvider::connect(token).await {
                Ok(p) => {
                    state.github.write().await.insert(id, Arc::new(p));
                }
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
                    Ok(p) => {
                        state.gitlab.write().await.insert(id, Arc::new(p));
                    }
                    Err(e) => eprintln!("gitbuddy: restoring gitlab session failed: {e}"),
                }
            }
            Provider::Codeberg => {
                let base_url = account
                    .base_url
                    .clone()
                    .unwrap_or_else(|| "https://codeberg.org".to_string());
                match CodebergProvider::connect(token, base_url).await {
                    Ok(p) => {
                        state.codeberg.write().await.insert(id, Arc::new(p));
                    }
                    Err(e) => eprintln!("gitbuddy: restoring codeberg session failed: {e}"),
                }
            }
        }
    }
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
    let id = account.id.clone();
    keychain::save(&account.id, &token)
        .await
        .map_err(|e| format!("keychain: {e}"))?;
    accounts::upsert(&app, account)?;
    state.github.write().await.insert(id, Arc::new(provider));
    let _ = app.emit(EVT_PROVIDER_CHANGED, ());
    aggregator::refresh_now(&state);
    Ok(viewer)
}

#[tauri::command]
pub async fn gh_status(
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
) -> Result<Option<Viewer>, String> {
    state.ensure_initialized(&app).await;
    // Legacy single-account status: returns the first connected GitHub
    // account's viewer if any. Replaced wholesale by `accounts_list` once
    // the per-account UI lands in the next frontend commit; until then the
    // existing settings screen calls this and shows one row.
    Ok(state
        .github
        .read()
        .await
        .values()
        .next()
        .map(|p| p.viewer.clone()))
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
            let id = account.id.clone();
            accounts::upsert(&app, account)?;
            state.github.write().await.insert(id, Arc::new(provider));
            let _ = app.emit(EVT_PROVIDER_CHANGED, ());
            aggregator::refresh_now(&state);
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
    // Legacy "disconnect from this provider entirely" semantics — wipes
    // every GitHub account at once, with a last-ditch legacy-key sweep for
    // any pre-migration install whose state somehow didn't get cleaned up
    // by `migrate_legacy_keychain`. Per-account disconnect lives in
    // `accounts_disconnect`; this command goes away once the UI migrates.
    let sweep = disconnect_all_for_provider(&state, &app, Provider::Github).await;
    let _ = keychain::delete(GH_LEGACY_KEY).await;
    let _ = app.emit(EVT_PROVIDER_CHANGED, ());
    aggregator::refresh_now(&state);
    sweep
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
    let id = account.id.clone();
    accounts::upsert(&app, account)?;
    // Keep gitlab_base_url in settings up to date — `ensure_initialized` no
    // longer reads it, but it's still consumed by the onboarding modal to
    // pre-fill the host suggestion next time the user reconnects.
    let mut s = settings::load(&app).unwrap_or_default();
    s.gitlab_base_url = Some(provider.base_url().to_string());
    settings::save(&app, &s)?;

    state.gitlab.write().await.insert(id, Arc::new(provider));
    let _ = app.emit(EVT_PROVIDER_CHANGED, ());
    aggregator::refresh_now(&state);
    Ok(viewer)
}

#[tauri::command]
pub async fn gl_status(
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
) -> Result<Option<GitLabStatus>, String> {
    state.ensure_initialized(&app).await;
    Ok(state
        .gitlab
        .read()
        .await
        .values()
        .next()
        .map(|p| GitLabStatus {
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
    let sweep = disconnect_all_for_provider(&state, &app, Provider::Gitlab).await;
    let _ = keychain::delete(GL_LEGACY_KEY).await;
    // Clear the base URL too so the next `+ Add` flow starts from
    // gitlab.com rather than re-suggesting the disconnected host.
    let mut s = settings::load(&app).unwrap_or_default();
    s.gitlab_base_url = None;
    settings::save(&app, &s)?;
    let _ = app.emit(EVT_PROVIDER_CHANGED, ());
    aggregator::refresh_now(&state);
    sweep
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
    let id = account.id.clone();
    accounts::upsert(&app, account)?;
    let mut s = settings::load(&app).unwrap_or_default();
    s.codeberg_base_url = Some(provider.base_url().to_string());
    settings::save(&app, &s)?;

    state.codeberg.write().await.insert(id, Arc::new(provider));
    let _ = app.emit(EVT_PROVIDER_CHANGED, ());
    aggregator::refresh_now(&state);
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
        .values()
        .next()
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
    let sweep = disconnect_all_for_provider(&state, &app, Provider::Codeberg).await;
    let _ = keychain::delete(CB_LEGACY_KEY).await;
    let mut s = settings::load(&app).unwrap_or_default();
    s.codeberg_base_url = None;
    settings::save(&app, &s)?;
    let _ = app.emit(EVT_PROVIDER_CHANGED, ());
    aggregator::refresh_now(&state);
    sweep
}

/// Per-account disconnect — the multi-account-aware primitive. The legacy
/// per-provider `*_disconnect` commands above call into this for each
/// matching account, and the upcoming Settings UI uses it directly with
/// one specific account_id.
///
/// Ordering is deliberate: Keychain delete *first*, registry *second*,
/// in-memory state *last*. If the Keychain delete fails (locked, permission
/// revoked, transient I/O), we bail before touching anything else so the
/// system stays consistent and the user can retry. A half-disconnect with
/// the registry wiped but the token still in the Keychain would leak the
/// secret indefinitely — `restore_from_accounts` only iterates the
/// registry, so the orphaned token would never get re-cleaned.
#[tauri::command]
pub async fn accounts_disconnect(
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
    account_id: String,
) -> Result<(), String> {
    keychain::delete(&account_id)
        .await
        .map_err(|e| format!("removing token from keychain failed: {e}"))?;
    accounts::remove(&app, &account_id)?;
    // Triple-remove on HashMaps is O(1) per call and avoids having to know
    // upfront which provider owns the id. Only one will actually hold it.
    state.github.write().await.remove(&account_id);
    state.gitlab.write().await.remove(&account_id);
    state.codeberg.write().await.remove(&account_id);
    let _ = app.emit(EVT_PROVIDER_CHANGED, ());
    aggregator::refresh_now(&state);
    Ok(())
}

/// Helper for the legacy per-provider disconnects: collect every id that
/// belongs to this provider (from both the in-memory HashMap and the
/// registry, in case they've drifted), then disconnect each. Doesn't emit
/// `provider-changed` itself — the caller emits once after the sweep.
///
/// Per-account ordering matches `accounts_disconnect`: Keychain first,
/// registry second, in-memory last. Unlike the single-account path, a
/// Keychain failure on one id doesn't abort the whole sweep — the failing
/// account is left intact and the other accounts still get cleaned. All
/// errors are collected and surfaced together so the UI can show the user
/// which accounts didn't disconnect cleanly.
async fn disconnect_all_for_provider(
    state: &AppState,
    app: &AppHandle,
    provider: Provider,
) -> Result<(), String> {
    let mut ids = std::collections::HashSet::new();
    match provider {
        Provider::Github => ids.extend(state.github.read().await.keys().cloned()),
        Provider::Gitlab | Provider::MpsdGitlab => {
            ids.extend(state.gitlab.read().await.keys().cloned())
        }
        Provider::Codeberg => ids.extend(state.codeberg.read().await.keys().cloned()),
    }
    if let Ok(file) = accounts::load(app) {
        for a in file.accounts {
            let matches = matches!(
                (a.provider, provider),
                (Provider::Github, Provider::Github)
                    | (Provider::Gitlab | Provider::MpsdGitlab, Provider::Gitlab)
                    | (
                        Provider::Gitlab | Provider::MpsdGitlab,
                        Provider::MpsdGitlab
                    )
                    | (Provider::Codeberg, Provider::Codeberg)
            );
            if matches {
                ids.insert(a.id);
            }
        }
    }
    let mut errors = Vec::new();
    for id in ids {
        if let Err(e) = keychain::delete(&id).await {
            errors.push(format!("keychain delete for {id}: {e}"));
            continue;
        }
        if let Err(e) = accounts::remove(app, &id) {
            errors.push(format!("registry remove for {id}: {e}"));
            continue;
        }
        match provider {
            Provider::Github => {
                state.github.write().await.remove(&id);
            }
            Provider::Gitlab | Provider::MpsdGitlab => {
                state.gitlab.write().await.remove(&id);
            }
            Provider::Codeberg => {
                state.codeberg.write().await.remove(&id);
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
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

/// List all connected accounts from `accounts.json`. The source of truth
/// is the registry — every set-token / oauth-poll command upserts here, so
/// the registry never lags the in-memory state. Account order is
/// preserved as-written (set-token / OAuth-poll append, so the result
/// reads chronologically by add date).
#[tauri::command]
pub async fn accounts_list(
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
) -> Result<Vec<Account>, String> {
    state.ensure_initialized(&app).await;
    Ok(accounts::load(&app)?.accounts)
}

// ── Aggregated data commands ───────────────────────────────────────────────
//
// These read from `AppState.cache`, populated by the backend aggregator loop
// (`aggregator::run_loop`). Pre-M6.5 every command did its own provider
// fan-out per call; centralising that into one timer means the popover and
// main window stay in sync without each pulling the same APIs separately,
// and it gives Phase 2's diff/notify code a single coherent snapshot to
// compare against.
//
// On a cold cache (first launch, before the first tick completes) these
// return empty Vecs. The frontend is fine with that — `data-updated` will
// fire as soon as the tick finishes and the windows re-read.

#[tauri::command]
pub async fn list_waiting(
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
) -> Result<Vec<WaitingItem>, String> {
    state.ensure_initialized(&app).await;
    Ok(state.cache.read().await.waiting.clone())
}

#[tauri::command]
pub async fn list_repos(
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
) -> Result<Vec<Repo>, String> {
    state.ensure_initialized(&app).await;
    Ok(state.cache.read().await.repos.clone())
}

#[tauri::command]
pub async fn list_releases(
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
) -> Result<Vec<Release>, String> {
    state.ensure_initialized(&app).await;
    Ok(state.cache.read().await.releases.clone())
}

#[tauri::command]
pub async fn list_ci(
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
) -> Result<Vec<CiRun>, String> {
    state.ensure_initialized(&app).await;
    Ok(state.cache.read().await.ci.clone())
}

/// Request an immediate aggregator tick. Returns as soon as the trigger is
/// queued; the actual fetch happens in the polling task and surfaces via
/// the `data-updated` event. Frontend's refresh button wires here.
#[tauri::command]
pub async fn aggregator_refresh_now(
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
) -> Result<(), String> {
    state.ensure_initialized(&app).await;
    aggregator::refresh_now(&state);
    Ok(())
}

/// Snapshot of the aggregator's last-sync metadata so a freshly-opened
/// window can hydrate its "Synced X ago" footer immediately, without having
/// to wait for the next tick.
#[derive(serde::Serialize)]
pub struct LastSyncInfo {
    pub synced_at: Option<String>,
    pub last_error: Option<String>,
}

#[tauri::command]
pub async fn last_sync_info(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<LastSyncInfo, String> {
    let cache = state.cache.read().await;
    Ok(LastSyncInfo {
        synced_at: cache.last_synced_at.clone(),
        last_error: cache.last_error.clone(),
    })
}

// ── Local index ─────────────────────────────────────────────────────────────

/// Clone a remote repo to `parent_dir/folder_name` and return the absolute
/// path of the new working directory.
///
/// Auth: when `account_id` is supplied (the frontend picks the most-recently-
/// added account that has access), the in-memory provider's token is fed to
/// libgit2 via a credentials callback. This avoids a fresh Keychain prompt —
/// the token is already in RAM from the initial restore. For public repos
/// the caller can pass `None` and the callback is skipped.
///
/// The actual clone runs on a blocking thread because libgit2 is synchronous
/// and a 200 MB checkout would otherwise stall the Tauri runtime.
#[tauri::command]
pub async fn clone_repo(
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
    url: String,
    parent_dir: String,
    folder_name: String,
    account_id: Option<String>,
) -> Result<String, String> {
    state.ensure_initialized(&app).await;

    // Boundary check: the credentials callback below hands the account token
    // to libgit2 for whatever URL we pass. If that URL were `http://` or
    // `git://`, the PAT would travel in clear. All URLs we receive in
    // practice come from a provider's API and are https://, but the frontend
    // input isn't trust-bounded — codify the assumption here. URL is not
    // echoed back on rejection to avoid surfacing potentially-credentialed
    // URLs (e.g. `https://user:pass@host/...`) in error toasts.
    let url = url.trim().to_string();
    if !url.starts_with("https://") {
        return Err("Clone URL must use https://.".into());
    }

    let folder_name = folder_name.trim();
    if folder_name.is_empty() {
        return Err("Folder name must not be empty.".into());
    }
    if folder_name.contains('/') || folder_name.contains('\\') {
        return Err("Folder name must not contain path separators.".into());
    }
    let parent = std::path::PathBuf::from(parent_dir.trim());
    if !parent.is_dir() {
        return Err(format!("Parent directory doesn't exist: {parent:?}"));
    }
    let target = parent.join(folder_name);
    if target.exists() {
        return Err(format!("Target already exists: {}", target.display()));
    }

    // Resolve the token (if any) from the in-memory provider whose
    // account_id matches. None for public clones; Some for authenticated.
    // If the account vanished between the frontend showing the button and
    // the click landing, fall through to anonymous and let libgit2 surface
    // a clear auth error.
    let token: Option<String> = if let Some(id) = account_id.as_deref() {
        if let Some(p) = state.github.read().await.get(id) {
            Some(p.token().to_string())
        } else if let Some(p) = state.gitlab.read().await.get(id) {
            Some(p.token().to_string())
        } else {
            state
                .codeberg
                .read()
                .await
                .get(id)
                .map(|p| p.token().to_string())
        }
    } else {
        None
    };

    tokio::task::spawn_blocking(move || {
        use git2::{Cred, FetchOptions, RemoteCallbacks};

        let mut callbacks = RemoteCallbacks::new();
        if let Some(tok) = token.clone() {
            callbacks.credentials(move |_url, _username, _allowed| {
                // "oauth2" works as the username for personal access tokens
                // on both GitLab and Gitea/Codeberg HTTPS endpoints, and is
                // accepted by GitHub too — saving us a per-provider switch.
                Cred::userpass_plaintext("oauth2", &tok)
            });
        }
        let mut fo = FetchOptions::new();
        fo.remote_callbacks(callbacks);

        let mut builder = git2::build::RepoBuilder::new();
        builder.fetch_options(fo);

        builder
            .clone(&url, &target)
            .map(|_| target.to_string_lossy().into_owned())
            .map_err(|e| format!("clone failed: {e}"))
    })
    .await
    .map_err(|e| format!("clone task panicked: {e}"))?
}

#[tauri::command]
pub async fn list_local_repos(
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
) -> Result<Vec<LocalRepo>, String> {
    state.ensure_initialized(&app).await;
    Ok(state.cache.read().await.locals.clone())
}

#[tauri::command]
pub fn get_settings(app: AppHandle) -> Result<Settings, String> {
    settings::load(&app)
}

#[tauri::command]
pub fn save_settings(
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
    settings: Settings,
) -> Result<(), String> {
    settings::save(&app, &settings)?;
    let _ = app.emit(EVT_SETTINGS_CHANGED, ());
    // The aggregator loop reads poll-interval (and, in Phase 2, notification
    // toggles) from settings — wake it so the new values take effect on the
    // current sleep cycle, not after the next tick.
    state.settings_reload.notify_one();
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
