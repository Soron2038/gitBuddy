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
    provider_util::ProviderBackend,
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

/// In-memory provider registry. One `HashMap` keyed by `Account.id` (the v2
/// `<provider>:<host>:<login>` string) holds every connected account across
/// all forge types as `Arc<dyn ProviderBackend>`, so the aggregator fan-out
/// and the auth/disconnect commands don't branch per provider. A single user
/// can hold multiple accounts per provider — e.g. two GitLab instances or a
/// personal + work GitHub — and each is restored / refreshed / removed
/// independently. The id's `<provider-slug>:` prefix (see
/// `accounts::provider_slug`) is how the legacy per-provider commands still
/// filter "all GitHub accounts" out of the unified map.
///
/// The `cache` + two `Notify`s drive the backend aggregator loop. Provider
/// commands here `refresh_trigger.notify_one()` after a connect/disconnect
/// so the cache repopulates immediately; `save_settings` notifies
/// `settings_reload` so the loop picks up a changed poll interval without
/// waiting out the current sleep.
pub struct AppState {
    pub providers: RwLock<HashMap<String, Arc<dyn ProviderBackend>>>,
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
            providers: RwLock::new(HashMap::new()),
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
        let stored = settings::load(app)
            .inspect_err(|e| {
                eprintln!("gitbuddy: settings load for legacy gitlab migration failed: {e} — the account can't be migrated without its base URL and will be skipped");
            })
            .ok();
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
        let stored = settings::load(app)
            .inspect_err(|e| {
                eprintln!("gitbuddy: settings load for legacy codeberg migration failed: {e} — falling back to the codeberg.org default host");
            })
            .ok();
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
/// The registry is populated in one write-lock acquisition at the end, so
/// concurrent readers never observe a half-restored registry.
async fn restore_from_accounts(app: &AppHandle, state: &AppState) {
    let file = match accounts::load(app) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("gitbuddy: reading accounts.json failed: {e}");
            return;
        }
    };

    let mut restored: Vec<(String, Arc<dyn ProviderBackend>)> = Vec::new();
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
                Ok(p) => restored.push((id, Arc::new(p) as Arc<dyn ProviderBackend>)),
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
                    Ok(p) => restored.push((id, Arc::new(p) as Arc<dyn ProviderBackend>)),
                    Err(e) => eprintln!("gitbuddy: restoring gitlab session failed: {e}"),
                }
            }
            Provider::Codeberg => {
                let base_url = account
                    .base_url
                    .clone()
                    .unwrap_or_else(|| "https://codeberg.org".to_string());
                match CodebergProvider::connect(token, base_url).await {
                    Ok(p) => restored.push((id, Arc::new(p) as Arc<dyn ProviderBackend>)),
                    Err(e) => eprintln!("gitbuddy: restoring codeberg session failed: {e}"),
                }
            }
        }
    }

    if !restored.is_empty() {
        let mut providers = state.providers.write().await;
        providers.extend(restored);
    }
}

// ── Provider auth commands ─────────────────────────────────────────────────

/// Connect (or re-validate) a PAT-authenticated account for any provider.
/// GitHub ignores `base_url`; GitLab and Codeberg/Gitea require it. The
/// connect → keychain → registry → settings order mirrors the old
/// per-provider trio: the token is only persisted after a successful connect,
/// and the registry insert is last so a failed keychain write never leaves an
/// in-memory provider whose secret didn't land.
#[tauri::command]
pub async fn provider_set_token(
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
    provider: Provider,
    token: String,
    base_url: Option<String>,
) -> Result<Viewer, String> {
    let (account, provider_box): (Account, Arc<dyn ProviderBackend>) = match provider {
        Provider::Github => {
            let p = GitHubProvider::connect(token.clone())
                .await
                .map_err(|e| e.to_string())?;
            let account =
                accounts::account_from(Provider::Github, &p.viewer, AuthMethod::Pat, None);
            (account, Arc::new(p))
        }
        Provider::Gitlab | Provider::MpsdGitlab => {
            let base_url = base_url
                .clone()
                .ok_or("GitLab needs a base URL (e.g. https://gitlab.com)")?;
            let p = GitLabProvider::connect(token.clone(), base_url)
                .await
                .map_err(|e| e.to_string())?;
            let account = accounts::account_from(
                Provider::Gitlab,
                &p.viewer,
                AuthMethod::Pat,
                Some(p.base_url().to_string()),
            );
            (account, Arc::new(p))
        }
        Provider::Codeberg => {
            let base_url = base_url
                .clone()
                .ok_or("Codeberg/Gitea needs a base URL (e.g. https://codeberg.org)")?;
            let p = CodebergProvider::connect(token.clone(), base_url)
                .await
                .map_err(|e| e.to_string())?;
            let account = accounts::account_from(
                Provider::Codeberg,
                &p.viewer,
                AuthMethod::Pat,
                Some(p.base_url().to_string()),
            );
            (account, Arc::new(p))
        }
    };

    let viewer = account.viewer.clone();
    let id = account.id.clone();
    keychain::save(&account.id, &token)
        .await
        .map_err(|e| format!("keychain: {e}"))?;
    accounts::upsert(&app, account)?;

    // Keep the per-provider base-URL hint in settings fresh so the next
    // onboarding modal pre-fills the host. `ensure_initialized` no longer
    // reads it, but the add-provider flow does. GitHub has no base URL.
    if let Some(base) = provider_box.base_url().map(str::to_string) {
        let mut s = settings::load(&app).unwrap_or_default();
        match provider {
            Provider::Gitlab | Provider::MpsdGitlab => s.gitlab_base_url = Some(base),
            Provider::Codeberg => s.codeberg_base_url = Some(base),
            Provider::Github => {}
        }
        settings::save(&app, &s)?;
    }

    state.providers.write().await.insert(id, provider_box);
    let _ = app.emit(EVT_PROVIDER_CHANGED, ());
    aggregator::refresh_now(&state);
    Ok(viewer)
}

/// First connected provider whose account id carries this provider's slug,
/// or `None`. The per-provider status commands use it to pull "the GitHub
/// account" / "the GitLab account" out of the unified registry.
async fn first_provider(state: &AppState, provider: Provider) -> Option<Arc<dyn ProviderBackend>> {
    let prefix = format!("{}:", accounts::provider_slug(provider));
    state
        .providers
        .read()
        .await
        .iter()
        .find(|(id, _)| id.starts_with(&prefix))
        .map(|(_, p)| p.clone())
}

/// Legacy single-account status for one provider: the first connected
/// account of that type, with its base URL (`None` for GitHub). Superseded by
/// `accounts_list` for the multi-account UI; the current settings/onboarding
/// screens still call this to show one row per provider.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ProviderStatus {
    pub viewer: Viewer,
    pub base_url: Option<String>,
}

#[tauri::command]
pub async fn provider_status(
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
    provider: Provider,
) -> Result<Option<ProviderStatus>, String> {
    state.ensure_initialized(&app).await;
    Ok(first_provider(&state, provider)
        .await
        .map(|p| ProviderStatus {
            viewer: p.viewer().clone(),
            base_url: p.base_url().map(str::to_string),
        }))
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
            state
                .providers
                .write()
                .await
                .insert(id, Arc::new(provider) as Arc<dyn ProviderBackend>);
            let _ = app.emit(EVT_PROVIDER_CHANGED, ());
            aggregator::refresh_now(&state);
            Ok(GhOAuthPollResult::Success { viewer })
        }
    }
}

fn oauth_http_client() -> Result<reqwest::Client, String> {
    crate::provider_util::http_client().map_err(|e| format!("http client: {e}"))
}

/// Disconnect every account of one provider at once — the "disconnect from
/// this provider entirely" action behind the settings screen. Per-account
/// disconnect is `accounts_disconnect`. Beyond the registry sweep this also
/// deletes the pre-migration single-account Keychain key and, for the
/// self-hostable forges, clears the persisted base-URL hint so the next
/// onboarding starts from the default host rather than the disconnected one.
#[tauri::command]
pub async fn provider_disconnect(
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
    provider: Provider,
) -> Result<(), String> {
    let sweep = disconnect_all_for_provider(&state, &app, provider).await;
    match provider {
        Provider::Github => {
            let _ = keychain::delete(GH_LEGACY_KEY).await;
        }
        Provider::Gitlab | Provider::MpsdGitlab => {
            let _ = keychain::delete(GL_LEGACY_KEY).await;
            let mut s = settings::load(&app).unwrap_or_default();
            s.gitlab_base_url = None;
            settings::save(&app, &s)?;
        }
        Provider::Codeberg => {
            let _ = keychain::delete(CB_LEGACY_KEY).await;
            let mut s = settings::load(&app).unwrap_or_default();
            s.codeberg_base_url = None;
            settings::save(&app, &s)?;
        }
    }
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
    // One registry now holds every provider, keyed by account id — a single
    // remove drops the right account regardless of which forge owns it.
    state.providers.write().await.remove(&account_id);
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
    // All provider ids share a `<slug>:` prefix (github:/gitlab:/codeberg:),
    // so one prefix match selects every account of this provider out of the
    // unified registry. `MpsdGitlab` collapses to the same `gitlab` slug.
    let slug_prefix = format!("{}:", accounts::provider_slug(provider));
    let mut ids = std::collections::HashSet::new();
    ids.extend(
        state
            .providers
            .read()
            .await
            .keys()
            .filter(|k| k.starts_with(&slug_prefix))
            .cloned(),
    );
    // And from accounts.json, in case the registry and the file have drifted.
    if let Ok(file) = accounts::load(app) {
        for a in file.accounts {
            if accounts::provider_slug(a.provider) == accounts::provider_slug(provider) {
                ids.insert(a.id);
            }
        }
    }
    let mut errors = Vec::new();
    let mut cleaned = Vec::new();
    for id in ids {
        if let Err(e) = keychain::delete(&id).await {
            errors.push(format!("keychain delete for {id}: {e}"));
            continue;
        }
        if let Err(e) = accounts::remove(app, &id) {
            errors.push(format!("registry remove for {id}: {e}"));
            continue;
        }
        cleaned.push(id);
    }
    // One write acquisition for the whole sweep — taking the lock once per
    // id inside the loop could interleave with (and starve against) a
    // concurrent tick holding the read lock.
    if !cleaned.is_empty() {
        let mut providers = state.providers.write().await;
        for id in &cleaned {
            providers.remove(id);
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
    //
    // When a token IS used, pin the clone to the account's own host: the
    // clone URL originates from a forge API response, and a malicious or
    // compromised instance could return a clone_url pointing at a foreign
    // HTTPS host to harvest the token the credentials callback hands out.
    let token: Option<String> = if let Some(id) = account_id.as_deref() {
        match state.providers.read().await.get(id) {
            Some(p) => {
                let expected = match p.base_url() {
                    Some(base) => {
                        url_host(base).ok_or_else(|| "Account base URL unparseable.".to_string())?
                    }
                    // GitHub provider carries no base URL — it is always
                    // github.com.
                    None => "github.com".to_string(),
                };
                if url_host(&url).as_deref() != Some(expected.as_str()) {
                    return Err(format!(
                        "Clone URL host does not match the account's host ({expected})."
                    ));
                }
                Some(p.token().to_string())
            }
            None => None,
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

/// Export portable configuration (settings only) to a user-chosen `path` as
/// pretty-printed JSON — the `settings.json` content (scan roots + ignore
/// patterns, editor/terminal commands, notification preferences, poll
/// interval, provider base URLs). The frontend picks `path` via a native save
/// dialog; the actual file write stays in the Rust core where the rest of the
/// persistence lives, so no extra fs plugin/capability is needed.
///
/// Accounts and tokens are deliberately excluded. Tokens live only in the
/// Keychain and never leave it; an account record without its secret can't be
/// restored into a working connection (there's no "needs re-auth" account
/// state), so on a new machine the user reconnects accounts — the base URLs
/// they need for self-hosted instances ride along in the settings above.
/// Start-at-login is a macOS LaunchAgent owned by the OS and likewise out of
/// band.
#[tauri::command]
pub async fn export_config(app: AppHandle, path: String) -> Result<(), String> {
    let settings = settings::load(&app)?;
    let json =
        serde_json::to_string_pretty(&settings).map_err(|e| format!("serialising config: {e}"))?;
    // Same crash-safe write path as settings.json itself, off the async
    // runtime — a slow target (network volume) must not stall a worker.
    tokio::task::spawn_blocking(move || {
        crate::util::atomic_write(std::path::Path::new(&path), json.as_bytes())
    })
    .await
    .map_err(|e| format!("export task panicked: {e}"))?
}

/// Import configuration previously produced by `export_config` from a
/// user-chosen `path`. Reads the file, parses it into `Settings` (rejecting
/// anything malformed), persists it through the same atomic-write + clamp path
/// `save_settings` uses, then wakes the aggregator so changed scan roots / poll
/// interval take effect immediately instead of on the next idle tick. Returns
/// the persisted (clamped) settings so the caller can refresh its own state
/// without a follow-up `get_settings`.
///
/// `editor_command`/`terminal_command` are deliberately NOT imported — they
/// name programs this machine will execute, and a shared config file must not
/// be able to plant those (see `settings::merge_imported`). The same
/// trust-boundary reasoning that keeps tokens out of the export applies.
#[tauri::command]
pub async fn import_config(
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
    path: String,
) -> Result<Settings, String> {
    let raw = tokio::task::spawn_blocking(move || {
        std::fs::read_to_string(&path).map_err(|e| format!("reading {path}: {e}"))
    })
    .await
    .map_err(|e| format!("import task panicked: {e}"))??;
    let imported: Settings =
        serde_json::from_str(&raw).map_err(|e| format!("parsing config: {e}"))?;
    let current = settings::load(&app)?;
    let settings = settings::merge_imported(&current, imported);
    settings::save(&app, &settings)?;
    let _ = app.emit(EVT_SETTINGS_CHANGED, ());
    state.settings_reload.notify_one();
    // Scan roots may have changed — kick an immediate tick so the local index
    // reflects the imported config without waiting out the poll interval.
    aggregator::refresh_now(&state);
    // Hand back the clamped, persisted form (poll interval pinned to band).
    settings::load(&app)
}

/// Run the user-configured editor command with `path` appended as the final
/// argument. The command is split on whitespace and spawned directly — no
/// shell. PATH lookup via execvp behaves exactly as `sh -c` did (neither
/// reads shell rc files), but shell metacharacters in a settings value are
/// no longer interpreted, so a tampered or hand-edited `editor_command`
/// can't smuggle extra commands. Flags still work: `"code --new-window"`
/// becomes `code --new-window <repo-path>`.
#[tauri::command]
pub async fn run_editor(app: AppHandle, path: String) -> Result<(), String> {
    let settings = settings::load(&app)?;
    let cmd = settings.editor_command.unwrap_or_default();
    let mut parts = cmd.split_whitespace().map(str::to_owned);
    let Some(program) = parts.next() else {
        return Err("No editor command configured. Set one in Settings.".into());
    };
    let args: Vec<String> = parts.collect();

    tokio::task::spawn_blocking(move || {
        std::process::Command::new(&program)
            .args(&args)
            .arg(&path)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("spawning editor `{program}` failed: {e}"))
    })
    .await
    .map_err(|e| format!("editor task panicked: {e}"))?
}

/// Open the user-configured terminal application at `path` via macOS
/// `open -a "<App>" <path>`, which launches (or reuses) that terminal with a
/// new session in the repo directory. Unlike `run_editor`, `terminal_command`
/// is an *application name* (e.g. "Terminal", "iTerm", "Warp"), not a shell
/// command — GUI terminals don't take a working directory as a positional CLI
/// arg, but `open -a` opens them in the given folder. Empty/unset → error
/// surfaced to the UI.
#[tauri::command]
pub async fn run_terminal(app: AppHandle, path: String) -> Result<(), String> {
    let settings = settings::load(&app)?;
    let app_name = settings.terminal_command.unwrap_or_default();
    let app_name = app_name.trim().to_string();
    if app_name.is_empty() {
        return Err("No terminal application configured. Set one in Settings.".into());
    }

    // `open` does its own argument handling, so we pass the app name and path
    // as separate args (no shell, no manual escaping needed).
    tokio::task::spawn_blocking(move || {
        std::process::Command::new("/usr/bin/open")
            .arg("-a")
            .arg(&app_name)
            .arg(&path)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("opening terminal failed: {e}"))
    })
    .await
    .map_err(|e| format!("terminal task panicked: {e}"))?
}

/// Host component of an `https://` URL: scheme, userinfo, port and path
/// stripped, lowercased. `None` when the input isn't https or has no host.
/// Hand-rolled (like `oauth::urlencode`) to avoid pulling in the `url` crate
/// for one field.
fn url_host(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host_port = authority.rsplit('@').next()?;
    let host = host_port.split(':').next()?;
    if host.is_empty() {
        None
    } else {
        Some(host.to_ascii_lowercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_host_extracts_and_normalises() {
        assert_eq!(
            url_host("https://GitHub.com/o/r.git").as_deref(),
            Some("github.com")
        );
        // Userinfo must not fool the host comparison…
        assert_eq!(
            url_host("https://github.com@evil.example/o/r.git").as_deref(),
            Some("evil.example")
        );
        // …nor may an explicit port.
        assert_eq!(
            url_host("https://gitlab.gwdg.de:8443/o/r.git").as_deref(),
            Some("gitlab.gwdg.de")
        );
        assert_eq!(
            url_host("https://codeberg.org").as_deref(),
            Some("codeberg.org")
        );
    }

    #[test]
    fn url_host_rejects_non_https_and_hostless() {
        assert_eq!(url_host("http://github.com/o/r"), None);
        assert_eq!(url_host("git://github.com/o/r"), None);
        assert_eq!(url_host("https:///path-only"), None);
        assert_eq!(url_host(""), None);
    }
}
