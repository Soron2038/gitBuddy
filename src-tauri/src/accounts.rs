//! Multi-account-ready account registry. Each connected account (GitHub PAT,
//! GitHub OAuth Device Flow, GitLab PAT on gitlab.com, GitLab PAT on
//! gitlab.gwdg.de, …) is one entry in `accounts.json`, with its secret
//! material stored separately in the Keychain under the same `id`.
//!
//! ## File version
//!
//! - **v1** (M6.3): ids shaped `<provider-slug>:<login>`. Worked while the UI
//!   was still single-account-per-provider; collides as soon as two
//!   self-hosted GitLab accounts share a login.
//! - **v2** (current): ids shaped `<provider-slug>:<host>:<login>`. Host is
//!   included for every provider — including GitHub (`github.com`) — so the
//!   scheme has no per-provider special case and stays stable if GitHub
//!   Enterprise ever lands.
//!
//! `commands::migrate_id_scheme_to_v2` upgrades v1 files on first launch,
//! moving each account's Keychain entry to the new id at the same time.

use crate::types::{Account, AuthMethod, Provider, Viewer};
use crate::util::atomic_write;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::{AppHandle, Manager};

const ACCOUNTS_FILE: &str = "accounts.json";
pub const CURRENT_VERSION: u32 = 2;

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

pub fn save(app: &AppHandle, file: &AccountsFile) -> Result<(), String> {
    let path = accounts_path(app)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir -p {parent:?}: {e}"))?;
    }
    let json =
        serde_json::to_string_pretty(file).map_err(|e| format!("serialising accounts: {e}"))?;
    atomic_write(&path, json.as_bytes())
}

/// Slug used as the first segment of `Account.id`. Kept stable and ASCII so
/// it survives round-trips through the macOS Security framework without
/// surprises. `MpsdGitlab` was a historical convenience variant; today it
/// shares the `gitlab` slug and is disambiguated entirely by host.
pub fn provider_slug(provider: Provider) -> &'static str {
    match provider {
        Provider::Github => "github",
        Provider::Gitlab | Provider::MpsdGitlab => "gitlab",
        Provider::Codeberg => "codeberg",
    }
}

/// Canonical host for a given (provider, base_url) pair.
///
/// - GitHub always reports `github.com` (we don't speak GitHub Enterprise yet,
///   but the id scheme can carry it without code changes when we do).
/// - GitLab / Codeberg parse the host out of the base URL — `https://gitlab.gwdg.de/api/v4`
///   collapses to `gitlab.gwdg.de`. Falls back to a `gitlab.com` /
///   `codeberg.org` default for the (defensive) case where base_url is
///   missing or unparseable, so we never end up writing a record with an
///   empty host segment.
pub fn account_host(provider: Provider, base_url: Option<&str>) -> String {
    match provider {
        Provider::Github => "github.com".to_string(),
        Provider::Gitlab | Provider::MpsdGitlab => parse_host(base_url, "gitlab.com"),
        Provider::Codeberg => parse_host(base_url, "codeberg.org"),
    }
}

fn parse_host(base_url: Option<&str>, fallback: &str) -> String {
    base_url
        .and_then(url_host)
        .unwrap_or_else(|| fallback.to_string())
        .to_lowercase()
}

/// Minimal host extractor. Avoids pulling in the `url` crate just for this —
/// base URLs are user-entered strings shaped like `https://gitlab.gwdg.de`
/// or `https://gitlab.gwdg.de/api/v4/`, both trivial to dissect by hand.
fn url_host(u: &str) -> Option<String> {
    let after_scheme = u.split_once("://").map(|(_, rest)| rest).unwrap_or(u);
    let host = after_scheme
        .split(['/', '?', '#'])
        .next()?
        .split('@')
        .next_back()?
        .split(':')
        .next()?;
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

/// Stable account identifier: `"<provider-slug>:<host>:<login-lowercase>"`.
/// Used as both the in-memory primary key and the Keychain account name.
/// Lowercasing the login guards against GitHub treating `Bjoernw` and
/// `bjoernw` as the same identity but serving different display casings on
/// different endpoints; lowercasing the host matches DNS semantics.
pub fn make_id(provider: Provider, host: &str, login: &str) -> String {
    format!(
        "{}:{}:{}",
        provider_slug(provider),
        host.to_lowercase(),
        login.to_lowercase()
    )
}

/// Build an `Account` record from a freshly-validated provider connection.
/// `base_url` is the authoritative host source — even though GitHub doesn't
/// surface it to the user today, passing `None` here still produces a
/// `github.com` host because of [`account_host`].
pub fn account_from(
    provider: Provider,
    viewer: &Viewer,
    auth: AuthMethod,
    base_url: Option<String>,
) -> Account {
    let host = account_host(provider, base_url.as_deref());
    Account {
        id: make_id(provider, &host, &viewer.login),
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
pub fn upsert(app: &AppHandle, account: Account) -> Result<(), String> {
    let mut file = load(app)?;
    file.accounts.retain(|a| a.id != account.id);
    file.accounts.push(account);
    save(app, &file)
}

/// Remove an account record by id. The Keychain entry under the same id is
/// the caller's responsibility — keep the two in lockstep at the call site
/// so we never leave dangling secrets.
pub fn remove(app: &AppHandle, id: &str) -> Result<(), String> {
    let mut file = load(app)?;
    file.accounts.retain(|a| a.id != id);
    save(app, &file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn make_id_includes_host_and_lowercases_segments() {
        assert_eq!(
            make_id(Provider::Github, "GitHub.com", "Bjoernw"),
            "github:github.com:bjoernw"
        );
        assert_eq!(
            make_id(Provider::Gitlab, "gitlab.gwdg.de", "user"),
            "gitlab:gitlab.gwdg.de:user"
        );
    }

    #[test]
    fn account_host_falls_back_per_provider() {
        assert_eq!(account_host(Provider::Github, None), "github.com");
        assert_eq!(
            account_host(Provider::Github, Some("ignored")),
            "github.com"
        );
        assert_eq!(account_host(Provider::Gitlab, None), "gitlab.com");
        assert_eq!(account_host(Provider::Codeberg, None), "codeberg.org");
    }

    #[test]
    fn url_host_handles_common_shapes() {
        assert_eq!(
            url_host("https://gitlab.gwdg.de"),
            Some("gitlab.gwdg.de".into())
        );
        assert_eq!(
            url_host("https://gitlab.gwdg.de/api/v4/"),
            Some("gitlab.gwdg.de".into())
        );
        assert_eq!(
            url_host("https://gitlab.mpsd.mpg.de/"),
            Some("gitlab.mpsd.mpg.de".into())
        );
        assert_eq!(
            url_host("https://user:pw@example.com:8080/x"),
            Some("example.com".into())
        );
        assert_eq!(url_host(""), None);
    }
}
