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

---

## 2026-06-01 — Provider trait + unified registry

Pre-polish refactor that finally retires the per-provider triplication the
PRD §6.2 flagged. `github.rs`, `gitlab.rs`, and `codeberg.rs` each owned a
full copy of the same shape — request/paginate/deserialize plus byte-
identical helper functions — and `AppState` carried three separate
`RwLock<HashMap<String, Arc<XProvider>>>` registries that every consumer
(aggregator fan-out, auth commands, disconnect, clone) had to branch across.

**`ProviderBackend` trait, list-only.** A single object-safe
`#[async_trait] trait ProviderBackend` exposes `viewer/token/base_url`
plus `list_waiting/list_repos/list_releases/list_ci`. Construction stays an
inherent `connect` on each concrete type — its signature differs per
provider (GitHub takes no base URL) and it returns `Self`, so it can't live
on an object-safe trait. `AppState` now holds one
`RwLock<HashMap<String, Arc<dyn ProviderBackend>>>`. Trait objects, not an
`enum AnyProvider`: enum dispatch would have re-introduced a 3-arm match per
method (the boilerplate we were deleting), and the per-tick cost is
dominated by HTTP, not vtable indirection. The id's `<provider-slug>:`
prefix (`accounts::provider_slug`) is how the legacy per-provider commands
still filter "all GitHub accounts" out of the one map.

**One `ProviderError`.** Replaced `GitHubError`/`GitLabError`/`CodebergError`
(near-identical, differing only in the auth-scope hint and whether the HTTP
error carried a base URL) with a single `provider_util::ProviderError`. Its
`HttpStatus { provider, base_url: Option<String>, status }` reproduces the
old per-provider Display strings; `Unauthorized(&'static str)` carries each
provider's scope hint. The aggregator now handles one error type.

**Fail-soft `list_waiting` everywhere.** GitHub and GitLab previously
aborted the whole "waiting" fetch if any one search scope failed (rate
limit, transient 5xx, panicked task); Codeberg already tolerated per-scope
failures and only propagated hard `Unauthorized`. Unified on the fail-soft
behaviour for all three — a menu-bar status app should degrade gracefully
rather than blank the list when one filter rate-limits. Hard auth errors
still propagate so the UI can prompt a reconnect.

**Command surface: 9 → 3.** `gh_/gl_/cb_set_token`, `_status`, `_disconnect`
collapsed into `provider_set_token` / `provider_status` /
`provider_disconnect`, each taking a `Provider`. OAuth Device Flow stays
GitHub-only (`gh_oauth_begin/poll`). This changed the JS↔Rust contract; the
`src/lib/data/api.ts` wrappers keep their old names (thin adapters) so the
two Svelte routes were untouched. The Keychain-first / registry-second /
in-memory-last disconnect ordering (the secret-leak guard) was preserved
exactly through the registry collapse.

**Shared helpers → `provider_util.rs`.** `humanise_age`, `reason_priority`,
`within_days`, and the GitHub-Actions `collapse_ci_status` (shared by GitHub
and Gitea/Codeberg) moved here. GitLab keeps its own
`collapse_pipeline_status` — its pipeline vocabulary genuinely differs, so
forcing a shared signature would be a contortion.

**Frontend stopped at dedup, deliberately.** Extracted `$lib/format.ts`
(date/path/host helpers) and `defaultSettings()`, deleted dead `stub.ts`,
and fixed a clutch of bugs (a `String.includes` host check that false-
matched `gitlab.com` against `lab.com`, a divergent "no sync yet"
placeholder, swallowed Keychain task panics now surfaced as real errors, a
silently-swallowed settings-load in legacy migration). Did **not** force the
main window and popover to share components/menus/state: on inspection the
two are divergent-by-design (compact popover vs. spacious main), not
mechanically duplicated, so a "shared" `RepoCard`/menu builder would either
change one window's copy or need so many props it saves nothing. The real
duplication (types, formatters, the settings literal) is gone; the rest is
intentional UI divergence and stays per-route.

## 2026-06-10 — Updater design (documenting the shipped v1.0 mechanism)

The in-app updater shipped with v1.0 but never got a DECISIONS entry; the
*how* lives in `docs/RELEASING.md`, this is the *why*.

**`tauri-plugin-updater` against a GitHub-Releases-hosted `latest.json`.**
The endpoint is `https://github.com/Soron2038/gitBuddy/releases/latest/
download/latest.json` — i.e. the manifest is just another release asset, no
server to run, no extra infrastructure to keep alive. Every release attaches
its own `latest.json`; the `releases/latest` indirection means installed
apps always read the newest one.

**Integrity = minisign signature, not the transport.** The app embeds the
minisign *public* key in `tauri.conf.json` (`plugins.updater.pubkey` —
public by design, committing it is correct) and refuses any artifact whose
`.sig` doesn't verify. A compromised GitHub account or a TLS middlebox can
therefore serve a manifest, but not a installable malicious build, without
the private key (held only in the maintainer's `~/.tauri/`, never in the
repo). This is why `bundle.createUpdaterArtifacts: true` makes
`TAURI_SIGNING_PRIVATE_KEY` a hard build requirement — an unsigned updater
artifact would be unshippable anyway. Local smoke-test builds opt out via
`scripts/build-app.sh --unsigned`.

**`latest.json` is generated, not hand-written** (since today —
`scripts/generate-latest-json.sh`). The manifest's `signature` field carries
the full `.sig` contents; hand-pasting it was the one release step where a
silent typo bricks auto-update for the entire installed base.

## 2026-06-10 — CI re-introduced (minimal), reversing 2026-06-05 removal

Commit `fd23bd1` removed the GitHub Actions workflow with the rationale
that local verification suffices for a single-author project and that the
macOS runner's 10× minute multiplier was the dominant cost. Two facts have
changed under that rationale:

1. **The repo is public** — the 10× multiplier only meters *private* repo
   billing; on public repos, hosted runners (including macOS) are free.
   The cost argument is void.
2. **Releases now auto-update user machines.** Since v1.0, a regression
   that reaches a release propagates to every installed copy via the
   updater. A pre-merge gate is cheap insurance against exactly that.

The new `.github/workflows/ci.yml` is deliberately minimal: type-check,
`cargo fmt --check`, `clippy -D warnings`, `cargo test --lib` on
`macos-latest` with `Swatinem/rust-cache` (vendored libgit2 + OpenSSL make
caching essential; warm runs should stay in single-digit minutes). It does
**not** build a Tauri bundle — that was the expensive, low-signal part of
the removed workflow, and release builds remain a local, signed,
tag-driven process per `docs/RELEASING.md`.

## 2026-06-10 — Main-window decomposition: DetailPane + RepoCard, no ListView

Follow-up to the 2026-06-01 "frontend stopped at dedup" entry, which scoped
*cross-window* sharing. This pass decomposed *within* the main window, with
every step verified visually (Playwright against the vite dev server, Tauri
IPC stubbed via `scripts/dev/tauri-ipc-stub.js` so fixture data makes
before/after screenshots comparable).

**Extracted:** `DetailPane.svelte` (pane chrome, quick actions, clone form,
CI/release/waiting sections — the parent recreates it per repo via `{#key}`,
which replaces the old clone-state-reset `$effect`) and `RepoCard.svelte`
(the former `repoCardEntry` snippet). `+page.svelte` went from ~4.5k to
~3.45k lines.

**Scoped-CSS coupling resolved by a window stylesheet, not duplication.**
The lists, cards and pane share a visual vocabulary (`.row` family,
`.kind-chip`, `.pchip`, `.rci`, badges). Svelte scoping doesn't cross
component boundaries, so those rules moved to `routes/main-window.css`,
imported once by the route — global *within this window's document* only.
Each Tauri window is its own webview/document, so this cannot leak into the
popover; component-scoped overrides still win on specificity.

**Deliberately NOT extracted: a generic ListView.** After the CSS moved to
the shared stylesheet, the remaining duplication is two ~40-line `{#each}`
blocks (waiting, releases — "local" turned out to be the RepoCard grid, not
a row list) whose content differs in every column. A shared component would
need chip/title/meta/trailing snippet props — the same props-explosion the
2026-06-01 entry rejected. Re-attempt only if a third row list appears.

The Settings view (~1k lines incl. the OAuth device-flow machine) is the
remaining large block and would be the natural next extraction; it was out
of scope here.

## 2026-06-11 — HTTP-level provider conformance tests (wiremock)

PRD §12 and CLAUDE.md had long flagged the same gap: the providers were only
covered by fixture-*deserialization* tests, never exercised through the HTTP
layer. The 1.0.2 release shipped behaviour changes (429 surfacing, per-page
pagination, the graceful-404 paths) with no automated regression coverage of
the request/response/error-mapping path.

**Added a `wiremock` dev-dependency and a per-provider conformance suite.**
Each provider is built via a new `#[cfg(test)] pub(crate) fn for_test` seam —
which skips `connect`'s network round-trip and, for GitLab/Codeberg, the
https-only base-URL normalisation — and pointed at a localhost mock server
over plain HTTP. The suite drives `list_repos` pagination + the short-page
stop condition, asserts the bearer header is sent, and checks the
401/429/5xx/graceful-404 mappings; GitHub additionally covers `list_waiting`'s
fail-soft tolerance (one search scope 500s, the rest succeed) and 401
propagation. Shared helpers (a `Viewer` stub, a pagination-page generator)
live in `provider_util::test_support`.

**GitHub's API base became a struct field.** It was a hardcoded `const
API_BASE`; the field (defaulting to that const, with `base` threaded through
the four free request fns) is the only production change, purely so the test
seam can redirect it to the mock server. `connect`'s signature is unchanged
and `base_url()` still returns `None`, so the clone-host check (GitHub ⇒
github.com, see the b7ffba6 fix) is untouched. GitLab/Codeberg already passed
`base` into their request helpers, so they needed only the `for_test`
constructor.

This closes the PRD §12 "provider trait conformance tests" item; the trait
itself landed 2026-06-01. Test count ~67 → ~87.
