# Review-Findings Fixes Implementation Plan

> **Status:** drafted + executed 2026-06-10 on `fix/review-findings` (24
> commits; kept as a record). Two deliberate deviations: (1) Task D1 shipped
> as a pure `deriveProviderHeads` helper in `$lib/data/auth.ts` instead of a
> full shared `stores.svelte.ts` — the windows' load semantics are
> divergent-by-design per DECISIONS.md 2026-06-01; (2) Task D2 (component
> extraction from the main window) was **not** executed: the list views
> share scoped CSS and the detail pane is positioned by the parent grid, so
> a mechanical split without visual QA risks silent styling regressions.
> Spun out as its own task to be done against the running app.

**Goal:** Fix all findings from the 2026-06-10 four-track codebase review (Rust robustness, frontend bugs, security hardening, docs/DX drift) in small, individually verified commits.

**Architecture:** No structural rewrites except (a) the `ProviderBackend` trait gains repo-passing signatures for `list_releases`/`list_ci`, (b) the aggregator tick becomes per-provider-parallel, and (c) shared frontend data loading moves into `src/lib/data/stores.svelte.ts` with component extractions from the 4.5k-line main page. Everything else is surgical.

**Tech Stack:** Rust (Tauri 2, tokio, reqwest, git2), Svelte 5 runes / SvelteKit 2 static, zsh build scripts.

**Verification gate (run at every phase boundary, from repo root):**

```bash
cd src-tauri && cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test --lib && cd ..
npm run check
```

**Branch:** all work on `fix/review-findings` (created from clean `main`). One commit per task.

---

## Phase A — Rust backend robustness

### Task A1: HTTP timeouts on all reqwest clients

**Files:** Modify `src-tauri/src/github.rs:39`, `src-tauri/src/gitlab.rs:37`, `src-tauri/src/codeberg.rs:33`, `src-tauri/src/commands.rs:618`.

- [ ] Add to each `Client::builder()` chain: `.connect_timeout(std::time::Duration::from_secs(10)).timeout(std::time::Duration::from_secs(30))`. Import `std::time::Duration` where missing. The three providers should share constants — put `pub(crate) const HTTP_CONNECT_TIMEOUT: Duration` / `HTTP_REQUEST_TIMEOUT: Duration` in `provider_util.rs` and use them in all four sites.
- [ ] Run gate. Commit `fix: add connect/request timeouts to all HTTP clients`.

### Task A2: race-proof `atomic_write` temp names

**Files:** Modify `src-tauri/src/util.rs`.

- [ ] Replace the fixed `path.with_extension("tmp")` with a unique name: `static COUNTER: AtomicU64` + `std::process::id()`, e.g. `format!("{stem}.{pid}.{n}.tmp")` placed next to the target (same dir ⇒ same volume ⇒ atomic rename). Keep the docstring contract.
- [ ] Add `#[cfg(test)]` test: two sequential `atomic_write` calls to the same target leave the new content and no `*.tmp` files behind.
- [ ] Run gate. Commit `fix: make atomic_write temp names collision-free`.

### Task A3: `export_config`/`import_config` — atomic + off the async runtime

**Files:** Modify `src-tauri/src/commands.rs:1010-1041`.

- [ ] `export_config`: make `async`, serialize, then `tokio::task::spawn_blocking` around `util::atomic_write(Path::new(&path), json.as_bytes())`.
- [ ] `import_config`: make `async`, wrap `std::fs::read_to_string` in `spawn_blocking`.
- [ ] Run gate. Commit `fix: export/import config use atomic_write and spawn_blocking`.

### Task A4: imported config must not carry executable commands (Security M2)

**Files:** Modify `src-tauri/src/commands.rs`, `src-tauri/src/settings.rs` (test module).

- [ ] Extract pure helper in `settings.rs`: `pub(crate) fn merge_imported(current: &Settings, imported: Settings) -> Settings` that takes everything from `imported` **except** `editor_command`/`terminal_command`, which are preserved from `current`.
- [ ] Test first (red): `merge_imported` keeps current editor/terminal commands and adopts imported scan roots/poll interval.
- [ ] Wire `import_config` through `merge_imported(&settings::load(&app)?, imported)`. Update both doc comments (export comment already explains secret exclusion — extend the same reasoning).
- [ ] Run gate. Commit `fix(security): imported config cannot inject editor/terminal commands`.

### Task A5: `run_editor` without `sh -c`

**Files:** Modify `src-tauri/src/commands.rs:1047-1072`.

- [ ] Split `editor_command` on whitespace: first token = program, rest = args, repo path appended as final arg. Spawn via `std::process::Command::new(program).args(args).arg(&path)` — no shell. PATH lookup via execvp is identical to what `sh -c` did; shell metacharacters stop being interpreted (that's the point). Update the doc comment; drop the now-unneeded quoting helper.
- [ ] Run gate. Commit `fix(security): run editor without a shell`.

### Task A6: Codeberg `pushed_at` mapping

**Files:** Modify `src-tauri/src/codeberg.rs` (`RawRepo` + mapping ~line 403, fixture tests).

- [ ] Test first: extend the repo fixture/test with distinct `"pushed_at"` and `"updated_at"`; assert `Repo.pushed_at` uses `pushed_at`, falling back to `updated_at` when absent.
- [ ] Add `pushed_at: Option<String>` to `RawRepo`; map `raw.pushed_at.unwrap_or(raw.updated_at)`.
- [ ] Run gate. Commit `fix: Codeberg repos sort by pushed_at, not metadata updated_at`.

### Task A7: remove dead `peel_to_commit` in local index

**Files:** Modify `src-tauri/src/local_index.rs:213-215`.

- [ ] Delete the `peel_to_commit` block and the `let _ = head_branch;` line; `local_oid` already feeds `graph_ahead_behind`.
- [ ] Run gate (local_index tests cover ahead/behind). Commit `refactor: drop dead peel_to_commit in ahead_behind`.

### Task A8: restructure `parse_poll` away from checked-unwrap

**Files:** Modify `src-tauri/src/oauth.rs:230-245`.

- [ ] Replace the `is_none()` guard + `unwrap()` with a single `match (raw.access_token, raw.token_type, raw.scope)` pattern. Existing oauth tests must stay green.
- [ ] Run gate. Commit `refactor: parse_poll uses match instead of checked unwrap`.

### Task A9: explicit 429 handling

**Files:** Modify `src-tauri/src/provider_util.rs` (`ProviderError`), `src-tauri/src/github.rs`, `src-tauri/src/gitlab.rs`, `src-tauri/src/codeberg.rs` (every status check).

- [ ] Add `ProviderError::RateLimited { provider: String }` (message: "rate limited — backing off until the next tick").
- [ ] In each provider's HTTP status handling, map `StatusCode::TOO_MANY_REQUESTS` (and GitHub's 403-with-`x-ratelimit-remaining: 0` if cheap to detect) to `RateLimited` before the generic `HttpStatus` fallback.
- [ ] Aggregator: when a fetch fails with `RateLimited`, record it in the snapshot error so the UI's `last_error` shows it instead of silently logging.
- [ ] Run gate. Commit `feat: surface 429 rate limiting distinctly`.

### Task A10: stop fetching the repo list three times per provider

**Files:** Modify `src-tauri/src/provider_util.rs:50-64` (trait), `src-tauri/src/github.rs`, `src-tauri/src/gitlab.rs`, `src-tauri/src/codeberg.rs`, `src-tauri/src/aggregator.rs`.

- [ ] Change trait signatures to `async fn list_releases(&self, repos: &[Repo])` and `async fn list_ci(&self, repos: &[Repo])`. Each impl drops its internal `self.list_repos().await?` and uses the passed slice (clone what it truncates today).
- [ ] `fetch_all` fetches repos once per provider and feeds releases/ci from that result.
- [ ] Run gate. Commit `perf: fetch repo list once per provider per tick`.

### Task A11: parallel per-provider fan-out in the aggregator

**Files:** Modify `src-tauri/src/aggregator.rs:360-415`.

- [ ] Replace the four sequential loops with one `tokio::spawn` per provider returning a per-provider partial snapshot:

```rust
let mut tasks = Vec::new();
for (id, p) in providers {
    tasks.push(tokio::spawn(async move {
        let (waiting, repos) = tokio::join!(p.list_waiting(), p.list_repos());
        let repos_ok = repos.as_deref().unwrap_or(&[]);
        let (releases, ci) = tokio::join!(p.list_releases(repos_ok), p.list_ci(repos_ok));
        (id, waiting, repos, releases, ci)
    }));
}
for t in tasks { /* await, tag_extend each Ok, eprintln + record each Err as today */ }
```

- [ ] Preserve existing ordering: sort `waiting` by `updated_at` desc and `repos` by `pushed_at` desc **after** merging all providers (as today).
- [ ] Run gate. Commit `perf: fetch all providers concurrently per tick`.

### Task A12: load settings once per tick

**Files:** Modify `src-tauri/src/aggregator.rs` (`run_loop`, `tick`, `fetch_all`).

- [ ] Load `Settings` once at the top of each loop iteration; pass `&Settings` down into `fetch_all` (local scan) and derive the poll interval from the same value instead of a second `settings::load`.
- [ ] Run gate. Commit `perf: single settings load per aggregator tick`.

### Task A13: lock hygiene in commands.rs

**Files:** Modify `src-tauri/src/commands.rs:367-414` (`restore_from_accounts`), `:730-745` (`disconnect_all_for_provider`).

- [ ] `restore_from_accounts`: build a local `Vec<(ProviderId, Arc<dyn ProviderBackend>)>`, then insert all entries under one `providers.write().await`.
- [ ] `disconnect_all_for_provider`: collect ids, then remove all under one `write()`.
- [ ] Run gate. Commit `fix: single lock acquisition for provider registry batch ops`.

### Task A14: bind clone credentials to the account's host (Security L2)

**Files:** Modify `src-tauri/src/commands.rs:884-964` (`clone_repo`).

- [ ] Before cloning, compute expected host: `base_url` host for GitLab/Codeberg accounts, `github.com` for GitHub. Parse the clone URL's host (manual `url` parsing — no new dep: strip `https://`, cut at first `/`, drop port and userinfo). If hosts differ → `Err("clone URL host X does not match account host Y")`.
- [ ] Unit-test the extracted host-check helper (mismatch, match, port, userinfo).
- [ ] Run gate. Commit `fix(security): refuse to send account token to a foreign clone host`.

### Task A15: dedupe `normalise_base_url`

**Files:** Modify `src-tauri/src/provider_util.rs`, `src-tauri/src/gitlab.rs:275-293`, `src-tauri/src/codeberg.rs:216-233`.

- [ ] Move the (byte-identical) function into `provider_util.rs` as `pub(crate) fn normalise_base_url`, delete both copies, keep one merged test set in `provider_util`'s test module (drop duplicated tests from the providers).
- [ ] Run gate. Commit `refactor: single normalise_base_url in provider_util`.

### Task A16: release profile

**Files:** Modify `src-tauri/Cargo.toml`.

- [ ] Append:

```toml
[profile.release]
lto = true
codegen-units = 1
strip = true
# panic = "abort" deliberately NOT set: libgit2 callbacks and the updater
# unwind across FFI; abort would turn recoverable errors into crashes.
```

- [ ] `cargo check --all-targets` (full release build not required). Commit `build: optimize release profile (LTO, strip)`.

---

## Phase B — Tauri security config

### Task B1: Content-Security-Policy

**Files:** Modify `src-tauri/tauri.conf.json:46`.

- [ ] First check whether templates load remote images (avatars): `grep -rn "<img" src/`. Include `https:` in `img-src` only if needed.
- [ ] Set production CSP (keep dev unrestricted via `"devCsp": null` so Vite HMR keeps working):

```json
"security": {
  "csp": "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline' https://api.fontshare.com https://fonts.googleapis.com; font-src 'self' https://cdn.fontshare.com https://fonts.gstatic.com; img-src 'self' asset: http://asset.localhost data: https:; connect-src 'self' ipc: http://ipc.localhost",
  "devCsp": null
}
```

  Font CDNs stay allowlisted (vendoring Fontshare fonts has license questions — documented as follow-up; JetBrains Mono vendoring optional later).
- [ ] Verify JSON parses (`python3 -m json.tool`), run gate. Commit `feat(security): production CSP for the webview`.

---

## Phase C — Frontend bug fixes

### Task C1: clear OAuth + refresh timers on unmount

**Files:** Modify `src/routes/+page.svelte` (onMount teardown ~line 735).

- [ ] In the `onMount` return callback add `resetOAuthState();` and clear `refreshSafetyHandle` if set.
- [ ] `npm run check`. Commit `fix: clear OAuth poll/countdown timers on unmount`.

### Task C2: popover Tauri listeners via onMount

**Files:** Modify `src/routes/popover/+page.svelte:503-556`.

- [ ] Convert the `$effect` registering `listen(...)` handlers to `onMount` with the same cancelled-flag + promise-unwrap teardown pattern the main window uses (`+page.svelte` onMount). No behavior change otherwise.
- [ ] `npm run check`. Commit `fix: register popover event listeners once via onMount`.

### Task C3: wire up the bell button

**Files:** Modify `src/routes/+page.svelte:1326-1337`.

- [ ] Add `onclick` switching the view to the waiting list (same state the "On you" nav uses — read the nav buttons and reuse exactly that assignment).
- [ ] `npm run check`. Commit `fix: bell button opens the waiting view`.

### Task C4: unique each-key for popover releases

**Files:** Modify `src/routes/popover/+page.svelte:977`.

- [ ] `{#each releases as r (r.repo_id + ':' + r.tag)}` — same compound key as the main window.
- [ ] `npm run check`. Commit `fix: popover release rows keyed by repo+tag`.

### Task C5: stop `$effect` resetting editor/terminal inputs

**Files:** Modify `src/routes/+page.svelte:94-100` and the settings-load path.

- [ ] Delete both mirror `$effect`s. Initialize `editorInput`/`terminalInput` exactly once after the first successful `getSettings()` (in the onMount load path, guarded by a `settingsInputsInitialised` flag).
- [ ] `npm run check`. Commit `fix: settings text inputs no longer reset while typing`.

### Task C6: cancellation guard in main-window data loading

**Files:** Modify `src/routes/+page.svelte` (`loadAllData`/`reloadFromCache`, ~lines 684-754).

- [ ] After each `await` that assigns to component state, bail if `cancelled` is set (pass a `() => cancelled` check or hoist the flag so both functions can read it).
- [ ] `npm run check`. Commit `fix: don't write fetched data into an unmounted main window`.

### Task C7: `repoAge` takes `nowMs`

**Files:** Modify `src/lib/format.ts:22-31`, call sites in both routes.

- [ ] Signature `repoAge(ts: string, nowMs: number)` mirroring `humaniseSync`; update call sites to pass the existing 1-second tick so ages refresh live.
- [ ] `npm run check`. Commit `fix: repo ages update with the clock tick`.

### Task C8: complete the popover tablist ARIA

**Files:** Modify `src/routes/popover/+page.svelte:810-840`.

- [ ] Give each tab `id="tab-<name>"` + `aria-controls="panel-main"`; give the panel `id="panel-main"` + `aria-labelledby={'tab-' + activeTab}`.
- [ ] `npm run check`. Commit `fix(a11y): link popover tabs to their panel`.

### Task C9: popover uses the multi-account API

**Files:** Modify `src/routes/popover/+page.svelte:240-263` (connect flow), `:519` (provider-changed handler); reference `src/routes/+page.svelte` `refreshAuth()` for the canonical derivation.

- [ ] Replace `ghStatus()/glStatus()/cbStatus()` refresh trio with one `accountsList()` call and derive the same view state the main window derives. The PAT connect path may keep `provider_set_token` (it returns the viewer) but must refresh via `accountsList()` afterwards.
- [ ] `npm run check`. Commit `fix: popover auth state follows the multi-account registry`.

---

## Phase D — Frontend decomposition

Goal: shared logic out of the two route monoliths, then component extractions. After each extraction: `npm run check` + visual sanity via `npm run build`. One commit per component.

### Task D1: shared data module

**Files:** Create `src/lib/data/stores.svelte.ts`; modify both `+page.svelte` files.

- [ ] Move the duplicated pieces both routes own today into one runes module: `$state` for `items/repos/releases/ciRuns/locals/settings/accounts/lastSyncedAt`, plus `loadAllData()`, `reloadFromCache()`, `refreshAuth()`, `openExternal()`, and the refresh-with-safety-timeout helper. (Each webview window is its own JS instance — this is code dedup, not cross-window state sharing; note that in the module docstring.)
- [ ] Both routes import from the module and delete their local copies.
- [ ] Gate + `npm run build`. Commit `refactor: shared data-loading module for both windows`.

### Task D2: extract main-window components

**Files:** Create `src/lib/components/RepoCard.svelte`, `WaitingList.svelte`, `RepoList.svelte`, `ReleaseList.svelte`, `CloneForm.svelte`, `DetailPane.svelte`; shrink `src/routes/+page.svelte`.

- [ ] Extract in this order (each with its scoped styles, props typed from `api.ts` types, callbacks as `on*` props): RepoCard (repo entry snippet + local-diag indicator), WaitingList (`status==='on-you'` block), RepoList (`status==='all'` block), ReleaseList (releases block), CloneForm, DetailPane (the `aside.detail-pane`, consuming CloneForm).
- [ ] One commit per component: `refactor: extract <Name> from main window`.
- [ ] Final size check: main page should land well under half its current 4508 lines.

---

## Phase E — Docs, build, DX

### Task E1: fix CLAUDE.md drift

**Files:** Modify `CLAUDE.md`.

- [ ] Provider section: providers ARE behind `ProviderBackend` (`provider_util.rs`) since commit `849e5a7`; commands are `provider_*` (+ `gh_oauth_*`). Update list_releases/list_ci signatures per Task A10.
- [ ] Testing section: replace "No test infrastructure exists yet" with reality (~60 lib tests incl. libgit2 fixtures; HTTP-fixture conformance layer still missing).
- [ ] Popover size 440×620. Release row: `npm run tauri build` requires `TAURI_SIGNING_PRIVATE_KEY` since `createUpdaterArtifacts: true`; add `scripts/build-app.sh` + `docs/RELEASING.md` pointers.
- [ ] Commit `docs: sync CLAUDE.md with shipped architecture`.

### Task E2: package metadata + lockfile sync

**Files:** Modify `package.json`; regenerate `package-lock.json`.

- [ ] Add `"engines": { "node": ">=20" }` and `"repository"` (github:Soron2038/gitBuddy). Run `npm install --package-lock-only`; verify lock root version becomes 1.0.1.
- [ ] Commit `chore: package metadata + lockfile version sync`.

### Task E3: build-app.sh fails fast / unsigned fallback

**Files:** Modify `scripts/build-app.sh`.

- [ ] Up-front check: if `TAURI_SIGNING_PRIVATE_KEY` unset → either exit with actionable message, or with new `--unsigned` flag build via `--config '{"bundle":{"createUpdaterArtifacts":false}}'` and skip notarization steps. Keep house style of the script.
- [ ] Commit `build: fail fast without signing key; add --unsigned local builds`.

### Task E4: generate latest.json by script

**Files:** Create `scripts/generate-latest-json.sh`; modify `docs/RELEASING.md`.

- [ ] Script reads version (from tauri.conf.json via python3 json), finds `release/*.app.tar.gz.sig`, emits `latest.json` with platform key `darwin-aarch64` (and `darwin-x86_64` if present), URL pattern from RELEASING.md, RFC3339 pub_date. `set -euo pipefail`, refuses to run if inputs missing.
- [ ] RELEASING.md: replace hand-write step with script invocation; add annotated-tag step (`git tag -a`) and "refresh lockfiles after bump" step.
- [ ] Commit `build: script latest.json generation`.

### Task E5: CHANGELOG.md

**Files:** Create `CHANGELOG.md`; modify `README.md` (link to it).

- [ ] Keep-a-Changelog format; seed v1.0.0 + v1.0.1 from `git tag -n99`/release commits; add Unreleased section listing this branch's user-visible fixes.
- [ ] Commit `docs: add CHANGELOG`.

### Task E6: tray-icon script fixes

**Files:** Modify `scripts/regenerate-tray-icon.py`.

- [ ] Fix docstring (SVG lives at `src-tauri/icons/tray-icon.svg`); print `OUT` directly instead of `OUT.relative_to(Path.cwd())` (ValueError off-root).
- [ ] `python3 -m py_compile scripts/regenerate-tray-icon.py`. Commit `fix: tray icon script path handling`.

### Task E7: .editorconfig

**Files:** Create `.editorconfig`.

- [ ] root=true; 2-space for ts/svelte/json/yaml, 4-space for rs/py, lf, final newline, trim trailing whitespace (except .md trailing-space).
- [ ] Commit `chore: add .editorconfig`.

### Task E8: DECISIONS.md appendices

**Files:** Modify `docs/DECISIONS.md` (append only!).

- [ ] New dated entry documenting the updater design (minisign key, `releases/latest/download/latest.json` endpoint, why createUpdaterArtifacts).
- [ ] If repo is public (verify: `curl -s -o /dev/null -w "%{http_code}" https://api.github.com/repos/Soron2038/gitBuddy` → 200), append a CI-revisit entry referencing `fd23bd1` (10× macOS multiplier only bills private repos) and add minimal `.github/workflows/ci.yml` (check+fmt+clippy+test with Swatinem/rust-cache); if private, skip the workflow, note in the final report instead.
- [ ] Commit `docs: DECISIONS entries for updater design (+ CI revisit if public)`.

### Task E9: small docs touch-ups

**Files:** Modify `docs/plans/main-window.md` (header says "awaiting execution" though shipped), `README.md` (releases/changelog pointer).

- [ ] Update plan header to "executed/shipped"; README gets a Releases/Changelog line. Screenshots need a running app — explicitly left as manual follow-up for the user.
- [ ] Commit `docs: status touch-ups`.

---

## Final verification

- [ ] Full gate (fmt, clippy, test, check) green.
- [ ] `npm run build` (frontend bundle) green.
- [ ] `git log --oneline main..` reads as one commit per task.
- [ ] Report: fixed list, skipped-with-reason list (font vendoring license check, README screenshots, full HTTP-fixture test layer, prettier reformat), merge offer.
