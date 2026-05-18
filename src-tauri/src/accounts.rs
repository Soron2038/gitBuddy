//! Multi-account-ready account registry. Each connected account (GitHub PAT,
//! GitHub OAuth Device Flow, GitLab PAT, …) is one entry in `accounts.json`,
//! with its secret material stored separately in the Keychain under the
//! same `id`.
//!
//! Schema is versioned (`version: 1`) so future migrations can detect old
//! files. The UI in this milestone is still single-account-per-provider —
//! the storage just stops actively preventing the multi-account case.

use crate::types::{Account, AuthMethod, Provider, Viewer};
use crate::util::atomic_write;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::{AppHandle, Manager};

const ACCOUNTS_FILE: &str = "accounts.json";
const CURRENT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountsFile {
    pub version: u32,
    pub accounts: Vec<Account>,
}

impl Default for AccountsFile {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            accounts: Vec::new(),
        }
    }
}

fn accounts_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("resolving app config dir: {e}"))?;
    Ok(dir.join(ACCOUNTS_FILE))
}

/// Read `accounts.json`, returning the empty default if the file doesn't
/// exist yet (first launch / fresh install / pre-M6.3 install before the
/// migration has run).
pub fn load(app: &AppHandle) -> Result<AccountsFile, String> {
    let path = accounts_path(app)?;
    if !path.exists() {
        return Ok(AccountsFile::default());
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("reading {path:?}: {e}"))?;
    serde_json::from_str(&raw).map_err(|e| format!("parsing {path:?}: {e}"))
}

#[allow(dead_code)] // wired up by the migration in the next commit
pub fn save(app: &AppHandle, file: &AccountsFile) -> Result<(), String> {
    let path = accounts_path(app)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir -p {parent:?}: {e}"))?;
    }
    let json =
        serde_json::to_string_pretty(file).map_err(|e| format!("serialising accounts: {e}"))?;
    atomic_write(&path, json.as_bytes())
}

/// Slug used both in `Account.id` and as the Keychain account key. Kept stable
/// and ASCII so it survives round-trips through the macOS Security framework
/// without surprises.
pub fn provider_slug(provider: Provider) -> &'static str {
    match provider {
        Provider::Github => "github",
        // Self-hosted GitLab installations share the same slug; the base_url
        // field carries the host distinction. Multi-account UI later will
        // need to include the host in the id to disambiguate two GitLab
        // accounts on different hosts, but with single-account-per-provider
        // UI that collision can't happen yet.
        Provider::Gitlab | Provider::MpsdGitlab => "gitlab",
        Provider::Codeberg => "codeberg",
    }
}

/// Stable account identifier: `"<provider-slug>:<login-lowercase>"`. Used as
/// both the in-memory primary key and the Keychain account name. Lowercasing
/// guards against GitHub treating `Bjoernw` and `bjoernw` as the same login
/// but serving different display casings on different endpoints.
pub fn make_id(provider: Provider, login: &str) -> String {
    format!("{}:{}", provider_slug(provider), login.to_lowercase())
}

/// Build an `Account` record from a freshly-validated provider connection.
/// Used by both the existing PAT commands (after the next commit's refactor)
/// and the OAuth Device Flow commands.
pub fn account_from(
    provider: Provider,
    viewer: &Viewer,
    auth: AuthMethod,
    base_url: Option<String>,
) -> Account {
    Account {
        id: make_id(provider, &viewer.login),
        provider,
        login: viewer.login.clone(),
        viewer: viewer.clone(),
        auth,
        base_url,
        added_at: chrono::Utc::now().to_rfc3339(),
    }
}

/// Upsert an account record into `accounts.json` by `id`. Used when adding
/// new accounts and when re-validating existing ones (the viewer info or
/// auth method may have changed — e.g. swapping a PAT for OAuth).
#[allow(dead_code)] // wired up by the PAT/OAuth command refactor in the next commit
pub fn upsert(app: &AppHandle, account: Account) -> Result<(), String> {
    let mut file = load(app)?;
    file.accounts.retain(|a| a.id != account.id);
    file.accounts.push(account);
    save(app, &file)
}

/// Remove an account record by id. The Keychain entry under the same id is
/// the caller's responsibility — keep the two in lockstep at the call site
/// so we never leave dangling secrets.
#[allow(dead_code)] // wired up by the disconnect-command refactor in the next commit
pub fn remove(app: &AppHandle, id: &str) -> Result<(), String> {
    let mut file = load(app)?;
    file.accounts.retain(|a| a.id != id);
    save(app, &file)
}
