//! GitHub OAuth Device Flow (RFC 8628).
//!
//! Device Flow rather than Authorization Code + PKCE because GitHub OAuth Apps
//! still require `client_secret` at the token exchange step even when PKCE is
//! used — shipping a real secret in a public desktop binary would be a fiction.
//! Device Flow needs only `client_id` (public) plus a code the user pastes in
//! the browser, which is exactly the trade-off we want for a single-author
//! macOS app distributed via GitHub Releases.
//!
//! The flow is two HTTP POSTs:
//!
//!   1. `begin_github` → `POST /login/device/code` → returns a `user_code` for
//!      the human to enter at `verification_uri`, a `device_code` for us to
//!      keep, an `expires_in` deadline, and an `interval` (seconds between
//!      polls — must be respected, GitHub will start failing with `slow_down`
//!      otherwise).
//!   2. `poll_github` → `POST /login/oauth/access_token` → one of five
//!      outcomes: `Pending`, `SlowDown(new_interval)`, `Denied`, `Expired`,
//!      or `Success(OAuthTokens)`. The Tauri command layer drives the poll
//!      cadence so the frontend can render a countdown and react to
//!      `SlowDown` without the backend having to emit events.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Public client ID of the registered GitHub OAuth App ("gitBuddy", owned by
/// Soron2038). Client IDs are not secrets — the Device Flow has no client
/// secret to protect, and this value appears in every device-code request
/// the binary makes. Rotate by registering a new app and replacing the
/// constant; old IDs stop working immediately.
pub const GITHUB_CLIENT_ID: &str = "Ov23liJmD8EPTQFQaiDc";

/// Scopes requested by `begin_github`. Matches what the existing PAT flow
/// needs:
///   - `repo` — private repos, CI runs, releases, all currently used in
///     [github.rs:list_repos] / `list_releases` / `list_ci`.
///   - `read:user` — the `/user` endpoint that `fetch_viewer` calls.
///   - `read:org` — `list_repos` requests `affiliation=organization_member`,
///     which is gated on this scope.
///
/// We deliberately do **not** request `notifications` — gitBuddy doesn't hit
/// the notifications endpoint today; if it ever does, the scope will be
/// added then.
const SCOPES: &str = "repo read:user read:org";

const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const ACCESS_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";

#[derive(Debug, Error)]
pub enum OAuthError {
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("unexpected response from GitHub: {0}")]
    BadResponse(String),
}

pub type Result<T> = std::result::Result<T, OAuthError>;

/// What `begin_github` returns to the Tauri command. `device_code` is a
/// session-scoped secret that the polling step needs; it round-trips through
/// the webview because the poll command is stateless on the backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

/// Persisted in the Keychain under the account's composite key for OAuth
/// accounts. Refresh tokens are deliberately not stored — GitHub OAuth Apps
/// configured for the default (non-expiring) Device Flow don't issue them.
/// If an org later enables user-to-server token expiration, the access_token
/// will start failing with 401 and the UI will surface a "reconnect" path,
/// same as for an expired PAT.
#[derive(Clone, Serialize, Deserialize)]
pub struct OAuthTokens {
    pub access_token: String,
    pub token_type: String,
    pub scope: String,
    pub obtained_at: String,
}

// Hand-rolled Debug redacts `access_token` so a future `dbg!(tokens)` or
// `eprintln!("{tokens:?}")` can't accidentally route a bearer token into
// stderr / Console.app / a panic message.
impl std::fmt::Debug for OAuthTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthTokens")
            .field(
                "access_token",
                &format!("<redacted, {} bytes>", self.access_token.len()),
            )
            .field("token_type", &self.token_type)
            .field("scope", &self.scope)
            .field("obtained_at", &self.obtained_at)
            .finish()
    }
}

/// Per-poll outcome, mapping RFC 8628 §3.5 error codes onto explicit
/// variants so the command layer can decide what to do without re-parsing
/// strings.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PollOutcome {
    Success(OAuthTokens),
    Pending,
    /// GitHub asks us to back off — interval is the new minimum seconds
    /// between polls. The frontend should adopt it for subsequent polls.
    SlowDown {
        interval: u64,
    },
    /// User clicked "Cancel" in the browser approval page.
    Denied,
    /// The device_code has passed its `expires_in` deadline without approval.
    Expired,
}

/// Kick off the flow. Returns the `user_code` for the human and the
/// `device_code` plus poll interval for the caller.
pub async fn begin_github(client: &reqwest::Client) -> Result<DeviceCodeResponse> {
    let body = format!(
        "client_id={}&scope={}",
        urlencode(GITHUB_CLIENT_ID),
        urlencode(SCOPES)
    );
    let response = client
        .post(DEVICE_CODE_URL)
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await?;
    let text = response.text().await?;
    parse_device_code(&text)
}

/// Single non-blocking poll. The Tauri command drives the cadence — the
/// frontend already needs to render a countdown anyway, so it's the natural
/// place to hold the timer state.
pub async fn poll_github(client: &reqwest::Client, device_code: &str) -> Result<PollOutcome> {
    let body = format!(
        "client_id={}&device_code={}&grant_type={}",
        urlencode(GITHUB_CLIENT_ID),
        urlencode(device_code),
        urlencode("urn:ietf:params:oauth:grant-type:device_code"),
    );
    let response = client
        .post(ACCESS_TOKEN_URL)
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await?;
    let text = response.text().await?;
    parse_poll(&text)
}

// ── Pure parsing helpers (unit-tested) ──────────────────────────────────────

fn parse_device_code(body: &str) -> Result<DeviceCodeResponse> {
    // Body is deliberately not echoed into the error — a malformed success
    // response could carry `user_code`/`device_code` which together complete
    // the auth flow. The serde error itself carries enough debug info.
    serde_json::from_str::<DeviceCodeResponse>(body)
        .map_err(|e| OAuthError::BadResponse(format!("device_code: {e}")))
}

/// GitHub returns 200 OK for both the in-progress error cases and the
/// final success — the `error` field tells them apart. We hand-decode here
/// rather than wedging it into a single serde enum because the success
/// shape has no `error` field and adding `#[serde(untagged)]` would silently
/// match the wrong arm on subtle response changes.
fn parse_poll(body: &str) -> Result<PollOutcome> {
    #[derive(Deserialize)]
    struct Raw {
        #[serde(default)]
        error: Option<String>,
        #[serde(default)]
        interval: Option<u64>,
        // Success-only fields:
        #[serde(default)]
        access_token: Option<String>,
        #[serde(default)]
        token_type: Option<String>,
        #[serde(default)]
        scope: Option<String>,
    }

    // As with parse_device_code, the body is not echoed — a partial-success
    // shape could contain `access_token`. Serde's error carries position info
    // that's enough to diagnose schema drift.
    let raw: Raw =
        serde_json::from_str(body).map_err(|e| OAuthError::BadResponse(format!("poll: {e}")))?;

    if let Some(err) = raw.error.as_deref() {
        return Ok(match err {
            "authorization_pending" => PollOutcome::Pending,
            "slow_down" => PollOutcome::SlowDown {
                // GitHub returns the new minimum interval; fall back to a
                // sane default if the field is missing.
                interval: raw.interval.unwrap_or(10),
            },
            "access_denied" => PollOutcome::Denied,
            "expired_token" => PollOutcome::Expired,
            other => {
                return Err(OAuthError::BadResponse(format!(
                    "unknown poll error: {other}"
                )))
            }
        });
    }

    // Report *which* fields are missing without echoing the body — the body
    // could carry the access_token even when other fields are absent.
    if raw.access_token.is_none() || raw.token_type.is_none() || raw.scope.is_none() {
        let mut missing = Vec::new();
        if raw.access_token.is_none() {
            missing.push("access_token");
        }
        if raw.token_type.is_none() {
            missing.push("token_type");
        }
        if raw.scope.is_none() {
            missing.push("scope");
        }
        return Err(OAuthError::BadResponse(format!(
            "success response missing fields: {}",
            missing.join(", ")
        )));
    }

    Ok(PollOutcome::Success(OAuthTokens {
        access_token: raw.access_token.unwrap(),
        token_type: raw.token_type.unwrap(),
        scope: raw.scope.unwrap(),
        obtained_at: Utc::now().to_rfc3339(),
    }))
}

/// Minimal form-encoder. The full `url` crate would pull in extra dependencies
/// for a use that boils down to escaping ASCII separators inside known-good
/// strings (client id, device code, scope list, grant type).
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_device_code_response() {
        let body = r#"{
            "device_code": "abc123",
            "user_code": "WDJB-MJHT",
            "verification_uri": "https://github.com/login/device",
            "expires_in": 900,
            "interval": 5
        }"#;
        let r = parse_device_code(body).unwrap();
        assert_eq!(r.device_code, "abc123");
        assert_eq!(r.user_code, "WDJB-MJHT");
        assert_eq!(r.verification_uri, "https://github.com/login/device");
        assert_eq!(r.expires_in, 900);
        assert_eq!(r.interval, 5);
    }

    #[test]
    fn parses_poll_pending() {
        let body = r#"{"error":"authorization_pending","error_description":"..."}"#;
        assert!(matches!(parse_poll(body).unwrap(), PollOutcome::Pending));
    }

    #[test]
    fn parses_poll_slow_down_with_interval() {
        let body = r#"{"error":"slow_down","interval":10}"#;
        match parse_poll(body).unwrap() {
            PollOutcome::SlowDown { interval } => assert_eq!(interval, 10),
            other => panic!("expected SlowDown, got {other:?}"),
        }
    }

    #[test]
    fn parses_poll_slow_down_without_interval_falls_back() {
        let body = r#"{"error":"slow_down"}"#;
        match parse_poll(body).unwrap() {
            PollOutcome::SlowDown { interval } => assert_eq!(interval, 10),
            other => panic!("expected SlowDown, got {other:?}"),
        }
    }

    #[test]
    fn parses_poll_denied() {
        let body = r#"{"error":"access_denied","error_description":"..."}"#;
        assert!(matches!(parse_poll(body).unwrap(), PollOutcome::Denied));
    }

    #[test]
    fn parses_poll_expired() {
        let body = r#"{"error":"expired_token"}"#;
        assert!(matches!(parse_poll(body).unwrap(), PollOutcome::Expired));
    }

    #[test]
    fn parses_poll_success() {
        let body = r#"{
            "access_token": "gho_abc",
            "token_type": "bearer",
            "scope": "repo,read:org,read:user"
        }"#;
        match parse_poll(body).unwrap() {
            PollOutcome::Success(t) => {
                assert_eq!(t.access_token, "gho_abc");
                assert_eq!(t.token_type, "bearer");
                assert_eq!(t.scope, "repo,read:org,read:user");
                assert!(!t.obtained_at.is_empty());
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[test]
    fn rejects_unknown_poll_error_code() {
        let body = r#"{"error":"surprise_party"}"#;
        assert!(matches!(parse_poll(body), Err(OAuthError::BadResponse(_))));
    }

    #[test]
    fn rejects_success_missing_fields() {
        let body = r#"{"access_token":"gho_abc"}"#;
        assert!(matches!(parse_poll(body), Err(OAuthError::BadResponse(_))));
    }

    #[test]
    fn urlencodes_special_chars() {
        assert_eq!(urlencode("hello world"), "hello%20world");
        assert_eq!(urlencode("a:b/c"), "a%3Ab%2Fc");
        assert_eq!(urlencode("plain-token_123.abc~"), "plain-token_123.abc~");
    }
}
