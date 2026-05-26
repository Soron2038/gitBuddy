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

## 2026-05-18 — Stable ad-hoc codesign for dev builds

**Decision:** Every `cargo run` on macOS detours through
`src-tauri/scripts/sign-and-run.sh`, which re-signs the freshly built
debug binary with a stable ad-hoc identifier
(`dev.soron2038.gitbuddy`) before exec'ing it. Wired up via
`src-tauri/.cargo/config.toml` under
`[target.'cfg(target_os = "macos")']`.

**Why:** Without the wrapper, macOS assigns each fresh `cargo build`
output a unique transient identifier. The Keychain binds its
"Always Allow" grants to that identifier, so every Rust-side rebuild
invalidated all previously granted permissions — six fresh prompts
on every relaunch during dev. Forcing the identifier to a stable
value makes the grants stick across rebuilds.

**Caveats:**

- Linux and Windows are unaffected — the cfg gate scopes the runner to
  macOS only.
- `tauri build` (release/bundle) doesn't go through this path; signing
  for production happens via Apple Developer ID and is M7's concern.
- The first launch after this lands still surfaces the Keychain
  prompts once. Click **Always Allow** on each; subsequent rebuilds
  shouldn't re-prompt.
- If the prompts ever come back, check that the binary's identifier
  is still stable with
  `codesign -d --verbose=4 src-tauri/target/debug/gitbuddy 2>&1 | grep Identifier=`.

## 2026-05-18 — No OAuth for GitLab or Codeberg (for now)

**Decision:** OAuth Device Flow stays GitHub-only. GitLab and
Codeberg / Gitea / Forgejo keep their existing PAT-only path. This
is removed from the "open candidates" list, not deferred.

**Why:**

- **gitlab.com**: would work cleanly (Device Flow GA in GitLab 17.9, same
  one-app-for-everyone shape as GitHub) but isn't actually relevant for
  the current user base — Björn and the colleagues he'd hand this to are
  on self-hosted gitlab.gwdg.de, not gitlab.com.
- **Self-hosted GitLab** (gitlab.gwdg.de etc.): each instance is its own
  OAuth realm. We can't pre-register a client_id for every possible
  instance, so the only paths are (a) hardcoded per-instance client_ids
  that don't scale, or (b) a "paste your own client_id" UI that pushes
  the OAuth-app registration onto every individual user. Both are worse
  UX than just pasting a PAT.
- **Codeberg / Gitea / Forgejo**: Device Flow isn't supported at all
  (confirmed via [Gitea docs](https://docs.gitea.com/development/oauth2-provider)
  — "only the Authorization Code Grant" — and an empirical 404 against
  `https://codeberg.org/login/oauth/device/code`). The only OAuth path
  available would be Authorization Code + PKCE, which means
  `tauri-plugin-deep-link` + a registered `gitbuddy://` URL scheme — the
  exact stack we rejected for GitHub in the previous decision. Doing it
  for just one provider would be a lot of new infra for a small win.

**Revisit when:** gitlab.com becomes a real use case for the user base,
*or* Gitea/Forgejo ship Device Flow support upstream.

## 2026-05-18 — OAuth App registration & rotation

The production OAuth App for gitBuddy (owner: Soron2038) is registered and
its `client_id` lives in `src-tauri/src/oauth.rs`. Client IDs are not
secrets — Device Flow has no client_secret to protect.

To **rotate** (suspected compromise, ownership change, fresh fork):

1. Go to <https://github.com/settings/applications/new>.
2. **Application name:** `gitBuddy`. **Homepage URL:** the repo URL.
   **Authorization callback URL:** anything — Device Flow ignores it
   (`https://github.com` is fine).
3. Create the app, then on the edit page tick **"Enable Device Flow"** and
   hit **Update application** — the checkbox is *separate* from the initial
   registration and the most common gotcha.
4. Copy the **Client ID** (current format `Ov23…`, older apps `Iv1.…`).
5. Drop it into `oauth::GITHUB_CLIENT_ID`, ship.

Old IDs stop working immediately on rotation; PAT auth is unaffected.

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

## 2026-05-26 — Backend polling loop + native notifications

**Decision:** The data-refresh loop moved from a `setInterval` in the
popover webview into a single tokio task on the Rust side
(`aggregator.rs`). The aggregator owns the cadence, owns the in-memory
cache, and owns the diff-vs-previous-snapshot work that drives native
notifications. The frontends became viewers: they subscribe to a
`data-updated` Tauri event and re-pull from cache; the previous
per-window `setInterval` blocks were removed.

**Why:** Polling-in-the-frontend worked while gitBuddy had one window,
but two windows polling the same APIs is wasted budget and the lack of a
single coherent "previous tick" snapshot blocked the notifications work
that closes PRD §4.8. Centralising lets one tick produce one snapshot,
one diff against persisted seen-state, and fan-out to both webviews.
The previous JS-side seen-state (`localStorage.gitbuddy:seen-waiting-ids`)
was dropped — backend now owns it.

**Implementation:**

- `src-tauri/src/aggregator.rs` — `run_loop` body. Two `tokio::sync::Notify`s
  let auth changes (`refresh_trigger`) and settings changes
  (`settings_reload`) cancel the current sleep, so a freshly-connected
  account or a slider-drag on the poll interval takes effect on the
  *current* sleep cycle rather than after.
- `src-tauri/src/notifications.rs` — `SeenStore` persisted to
  `notifications.json` next to `settings.json`. Kept as a separate file
  rather than folded into settings: settings is user-edited and
  read-mostly; the seen-store is opaque churn that mutates every tick,
  and bundling them risks corrupting user preferences if a write at tick
  time races with a Settings save.
- `src-tauri/src/settings.rs` — schema v1 → v2 migration. v1's flat
  `notifications_enabled` bool became `notifications.enabled` inside a
  struct that also carries Do-Not-Disturb + per-event toggles, plus a
  new `poll_interval_minutes` (clamped 1..=60). Migration is silent and
  one-shot: `load()` rewrites the file in v2 form before returning, so
  the on-disk shape is canonical from the next read onward.

**Cold-start guard:** the very first tick after install seeds every
currently-visible item as already-seen and flips the `initialised` flag,
firing nothing. Without this an upgrade or fresh install would blast the
notification centre with the user's entire backlog of assignments and
release tags. The guard cost is a single boolean on disk; the alternative
("notify everything on first launch, sorry") would have been a regression.

**Per-provider CI actor:** the fourth PRD event ("CI failure where the
viewer authored the latest commit/PR") needs each provider to surface a
login for the run's triggerer. GitHub exposes `actor.login` on workflow
runs, GitLab exposes `user.username` on pipelines, Gitea ≥1.21 exposes
`actor.login` matching GitHub, older Gitea uses `triggered_by.username`,
and Forgejo variants ship `actor_user.username`. `codeberg.rs::RawRun`
uses `serde(alias = "triggered_by", alias = "actor_user")` plus
`RunActor`'s `serde(alias = "username")` to accept all four shapes; a
self-hosted Forgejo that doesn't expose the actor at all parses as
`None` and the CI-failure notification path silently skips. Live-
verified against codeberg.org but not every self-hosted variant — if a
Forgejo user reports missing CI notifications, separate fix.

**Re-run attribution edge case:** on GitHub, when someone other than
the original author clicks "Re-run failed jobs", the workflow run's
`actor` becomes the re-runner. The notification will go to the re-runner
rather than the original author. Accepted: PRD-conformant, easy to
explain, and any "stable across re-runs" attribution would need pulling
the head commit's author separately on every CI fetch — too much for
the value.

**`Authored` waiting-reason notifies:** kept the existing JS behaviour
where the `authored` reason (commits / replies on your own PR) fires
notifications. PRD §4.8 lists assigned/review/mention but not authored;
the prior behaviour was already shipping and silencing it would be a
visible regression. Per-event toggle is the user-facing escape hatch.

**Polling cadence default:** 5 minutes, matching the legacy
`POLL_INTERVAL_MS = 5 * 60 * 1000` constant. Clamp band 1..=60 so a
hand-edited config file can't take the loop to 0-second hammering or
hour-plus idle. The `Notify`-based wake makes a slider drag in the UI
take effect immediately.

**Click-to-open from notifications:** out of scope for this iteration.
`tauri-plugin-notification` provides a click event but the payload
roundtrip is limited on macOS 14+, and building a sidecar
`pending_clicks: HashMap<id, url>` plus a `NSDelegate` would have
doubled the scope. Notifications are informative; the user clicks the
popover for action. Revisit if it becomes a real ask.
