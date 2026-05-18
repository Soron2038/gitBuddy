# Decisions

Short rationale notes for choices that aren't self-evident from the code or
the PRD. Append-only — entries get a date and a section header. If an old
decision is reversed later, leave it standing and add a new entry pointing
back at it, so future code archaeology can follow the trail.

---

## 2026-05-18 — GitHub OAuth: Device Flow, not PKCE

**Decision:** GitHub's "Sign in with browser" path uses the OAuth Device Flow
(RFC 8628). Authorization Code + PKCE was explicitly rejected.

**Why:** GitHub OAuth Apps still require `client_secret` at the token
exchange step even when PKCE is in play (see
`docs.github.com/apps/oauth-apps/building-oauth-apps/authorizing-oauth-apps`).
Shipping a real secret in a public desktop binary is fiction — extractable
with `strings` in seconds — so PKCE would buy us nothing beyond a slightly
prettier redirect dance and a chunk of new infra (`tauri-plugin-deep-link`,
custom `gitbuddy://` URL scheme registered in the app bundle, loopback
fallback). Device Flow needs only the `client_id`, which is genuinely public,
and trades one UX step (user pastes a short code at
`github.com/login/device`) for vastly simpler internals.

PAT auth remains supported in parallel — Device Flow is the new default but
not the only option.

**Implementation:**
- `src-tauri/src/oauth.rs` — pure-Rust two-call client, no new dependency
  beyond `reqwest` (already in the tree). Parsing helpers are unit-tested.
- `src-tauri/src/commands.rs::gh_oauth_begin` / `gh_oauth_poll` — Tauri
  commands. The poll is one HTTP call per invocation; the frontend drives
  the cadence and respects the `slow_down` interval bumps.

## 2026-05-18 — Maintainer setup: register the OAuth App

The Device Flow needs a registered OAuth App on github.com to get a
`client_id`. One-time setup per maintainer:

1. Go to https://github.com/settings/applications/new.
2. **Application name:** `gitBuddy`. **Homepage URL:** the repo URL.
   **Authorization callback URL:** anything — Device Flow ignores it
   (`https://github.com` is fine).
3. Create the app, then in its settings tick **"Enable Device Flow"**.
4. Copy the **Client ID** (looks like `Iv1.…` or `Ov23…`).
5. Drop it into `src-tauri/src/oauth.rs` replacing the
   `TODO_FROM_OAUTH_APP_REGISTRATION` placeholder in `GITHUB_CLIENT_ID`.

Until step 5 lands, the OAuth button surfaces a clear
"OAuth App client ID not configured" error rather than 404'ing against
GitHub's API. PAT auth is unaffected.

If the client ID ever leaks publicly: rotate it on the same settings page,
update `GITHUB_CLIENT_ID`, ship. The Device Flow has no client secret to
worry about.

## 2026-05-18 — Keychain layout: per-account composite keys

**Decision:** Each connected account lives in its own Keychain entry, keyed
by `<provider-slug>:<login-lowercase>` (e.g. `github:bjoernw`). PAT accounts
store the bare token string; OAuth accounts store a JSON `OAuthTokens` blob
(`access_token`, `token_type`, `scope`, `obtained_at`).

**Why:** The pre-M6.3 layout had one flat entry per provider type
(`"github"`, `"gitlab"`, `"codeberg"`), which can't represent two accounts
on the same provider, can't carry OAuth scope/expiry metadata alongside the
token, and conflates "what's in the keychain" with "what kind of auth is
this" in a way that surprises during disconnect. Composite keys keep the
secret-material storage flat and ASCII while letting the registry
(`accounts.json`) carry all the non-secret metadata.

The multi-account *UI* is still single-account-per-provider — the storage
layer is just no longer the limiting factor.

**Migration:** `AppState::ensure_initialized` runs a one-shot upgrade on
first launch of the new build. For each legacy key that exists and isn't yet
represented in `accounts.json`: connect with the legacy token, derive the
composite key, write the token under the new key, append the `Account`
record, then delete the legacy key. If the legacy token is revoked, the
entry is left in place for a later retry — no destructive cleanup before
the migration confirms success.

**Account record (`accounts.json`):** versioned at `1`, holds
`{ id, provider, login, viewer, auth: "pat" | "oauth_device", base_url, added_at }`
per account. Atomic write via the shared `util::atomic_write` helper, same
shape as `settings.json`.

**Refresh tokens:** not modelled. GitHub Device Flow Apps issue non-expiring
access tokens by default. If an org enables "User-to-server token
expiration", the access token starts returning 401 and the UI surfaces the
same reconnect path it already shows for revoked PATs. We'll model refresh
the day this materially bites — adding it now would be carrying complexity
for a case that may never happen.
