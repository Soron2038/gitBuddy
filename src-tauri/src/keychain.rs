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

/// The three Keychain operations, behind a trait so the account migrations can
/// be driven against an in-memory fake.
///
/// Those migrations are the most stateful code in the app — they interleave
/// Keychain reads, writes and deletes with `accounts.json` — and were entirely
/// untested, because reaching them meant a real Keychain and a Tauri
/// `AppHandle`. That is where the id-scheme migration's partial-failure
/// handling lives, and getting it wrong loses a user's stored token.
#[async_trait::async_trait]
pub trait KeychainStore: Send + Sync {
    async fn load(&self, key: &str) -> keyring::Result<Option<String>>;
    async fn save(&self, key: &str, secret: &str) -> keyring::Result<()>;
    async fn delete(&self, key: &str) -> keyring::Result<()>;
}

/// The real thing: delegates to the free functions above.
pub struct SystemKeychain;

#[async_trait::async_trait]
impl KeychainStore for SystemKeychain {
    async fn load(&self, key: &str) -> keyring::Result<Option<String>> {
        load(key).await
    }
    async fn save(&self, key: &str, secret: &str) -> keyring::Result<()> {
        save(key, secret).await
    }
    async fn delete(&self, key: &str) -> keyring::Result<()> {
        delete(key).await
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::collections::{HashMap, HashSet};
    use std::sync::Mutex;

    /// In-memory `KeychainStore` for tests, with per-key failure injection so
    /// the partial-failure branches of the migrations can be exercised.
    #[derive(Default)]
    pub(crate) struct FakeKeychain {
        entries: Mutex<HashMap<String, String>>,
        /// Keys whose `load` should fail (simulating a locked Keychain or a
        /// denied permission dialog).
        fail_load: Mutex<HashSet<String>>,
        /// Keys whose `save` should fail.
        fail_save: Mutex<HashSet<String>>,
        /// Every delete that was actually issued, in order — lets a test assert
        /// that a *failed* migration didn't drop the old entry.
        pub(crate) deleted: Mutex<Vec<String>>,
    }

    impl FakeKeychain {
        pub(crate) fn with(entries: &[(&str, &str)]) -> Self {
            let this = Self::default();
            for (k, v) in entries {
                this.entries
                    .lock()
                    .expect("lock")
                    .insert((*k).to_string(), (*v).to_string());
            }
            this
        }
        pub(crate) fn fail_load_for(&self, key: &str) {
            self.fail_load.lock().expect("lock").insert(key.to_string());
        }
        pub(crate) fn fail_save_for(&self, key: &str) {
            self.fail_save.lock().expect("lock").insert(key.to_string());
        }
        pub(crate) fn get(&self, key: &str) -> Option<String> {
            self.entries.lock().expect("lock").get(key).cloned()
        }
        pub(crate) fn keys(&self) -> HashSet<String> {
            self.entries.lock().expect("lock").keys().cloned().collect()
        }
    }

    #[async_trait::async_trait]
    impl KeychainStore for FakeKeychain {
        async fn load(&self, key: &str) -> keyring::Result<Option<String>> {
            if self.fail_load.lock().expect("lock").contains(key) {
                return Err(keyring::Error::Invalid("load".into(), key.into()));
            }
            Ok(self.entries.lock().expect("lock").get(key).cloned())
        }
        async fn save(&self, key: &str, secret: &str) -> keyring::Result<()> {
            if self.fail_save.lock().expect("lock").contains(key) {
                return Err(keyring::Error::Invalid("save".into(), key.into()));
            }
            self.entries
                .lock()
                .expect("lock")
                .insert(key.to_string(), secret.to_string());
            Ok(())
        }
        async fn delete(&self, key: &str) -> keyring::Result<()> {
            self.deleted.lock().expect("lock").push(key.to_string());
            self.entries.lock().expect("lock").remove(key);
            Ok(())
        }
    }
}
