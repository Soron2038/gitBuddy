//! Credential storage backed by the macOS Keychain (via the `keyring` crate).
//!
//! All operations are wrapped in `spawn_blocking` because the underlying
//! Security framework calls are synchronous and can prompt the user — we
//! mustn't block Tauri's async runtime threads waiting for a Keychain
//! permission dialog.

use keyring::Entry;

const SERVICE: &str = "dev.soron2038.gitbuddy";

/// Save `token` for the given `account` key (e.g. `"github"` for the single
/// GitHub account supported in M2; later expanded to e.g. `"github:work"`).
pub async fn save(account: &str, token: &str) -> keyring::Result<()> {
    let account = account.to_owned();
    let token = token.to_owned();
    tokio::task::spawn_blocking(move || {
        let entry = Entry::new(SERVICE, &account)?;
        entry.set_password(&token)
    })
    .await
    .map_err(join_failure)?
}

/// Load a previously stored token. Returns `Ok(None)` if no entry exists,
/// `Err(_)` for any other failure.
pub async fn load(account: &str) -> keyring::Result<Option<String>> {
    let account = account.to_owned();
    tokio::task::spawn_blocking(
        move || match Entry::new(SERVICE, &account)?.get_password() {
            Ok(p) => Ok(Some(p)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e),
        },
    )
    .await
    .map_err(join_failure)?
}

pub async fn delete(account: &str) -> keyring::Result<()> {
    let account = account.to_owned();
    tokio::task::spawn_blocking(
        move || match Entry::new(SERVICE, &account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e),
        },
    )
    .await
    .map_err(join_failure)?
}

/// A panic or cancellation inside the `spawn_blocking` Keychain task surfaces
/// here as a `JoinError`. Rather than `.expect()` (which would unwind into
/// Tauri's runtime and take the whole app down on a Security-framework
/// hiccup), wrap it as a normal `keyring::Error` so the caller handles it
/// like any other Keychain failure.
fn join_failure(e: tokio::task::JoinError) -> keyring::Error {
    keyring::Error::PlatformFailure(Box::new(e))
}
