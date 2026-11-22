# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

gitBuddy is a **macOS menu-bar companion** for GitHub, GitLab, and Codeberg/Gitea/Forgejo, with a **Windows x64 port** in preview (tray app, NSIS installer). It aggregates repos, issues/PRs, releases, and CI status across multiple forge accounts, and surfaces the state of local clones (via libgit2). Tauri 2 shell + SvelteKit 2 / Svelte 5 frontend + Rust core. The Windows port's plan and progress: `docs/plans/2026-11-21-windows-port.md`.

Scope and milestones live in `PRD.md`. Architecture decisions with dated rationales live in `docs/DECISIONS.md` (append-only — never edit past entries; add a new entry that references the older one if reversing course).

## Commands

All commands run from repo root unless noted.

| Task | Command |
|------|---------|
| Dev (Tauri + Vite) | `npm run tauri dev` |
| Frontend type-check | `npm run check` |
| Frontend tests | `npm run test` (watch: `npm run test:watch`) |
| Frontend gate (check + tests) | `npm run verify` |
| Frontend type-check (watch) | `npm run check:watch` |
| Rust check | `cd src-tauri && cargo check --all-targets` |
| Rust lint | `cd src-tauri && cargo clippy --all-targets -- -D warnings` |
| Rust format check | `cd src-tauri && cargo fmt --all -- --check` |
| Rust format apply | `cd src-tauri && cargo fmt --all` |
| Rust tests | `cd src-tauri && cargo test --lib` |
| Frontend bundle (no Tauri) | `npm run build` |
| Release build (signed) | `scripts/build-app.sh` — see `docs/RELEASING.md` |
| Release (macOS + Windows) | `scripts/release.sh` — see `docs/RELEASING.md` |
| Windows installer, local unsigned | `npm run tauri build -- --bundles nsis --config '{"bundle":{"createUpdaterArtifacts":false}}'` (Git Bash; see README) |
| Windows editor/terminal smoke test | `GITBUDDY_SMOKE_EDITOR=code GITBUDDY_SMOKE_TERMINALS='wt\|cmd' cargo test --lib launcher::tests::smoke -- --ignored` (opens real windows) |
| Tray icon regen | `python3 scripts/regenerate-tray-icon.py` (`python` on Windows; needs Pillow) |
| Palette contrast check | `node scripts/check-contrast.mjs` (add `--html design-mockups/04-buddy-warm-dark.html` for the mockup's palettes) |

CI (`.github/workflows/ci.yml`, push/PR to `main`) runs a minimal gate:
- `npm run check` on `ubuntu-latest`;
- on `macos-latest`: `cargo fmt --check`, `cargo clippy --all-targets -D warnings` and `cargo test --lib`;
- the same clippy and tests on `windows-latest` (`backend-windows`). That job is the only place the Windows side of the `cfg` gates compiles if you work on a Mac.

No Tauri bundle build there: the Mac release stays local and signed (2026-06-10 entry in `docs/DECISIONS.md`). The Windows installer is built by `.github/workflows/release-windows.yml` on `v*` tags. That workflow also runs, unsigned, on any PR that changes it (2026-11-22 entry). It installs the current `stable` Rust, which can be newer than your local one: new Clippy lints show up there first. Reproduce with that exact version side by side (`rustup toolchain install <ver> -c clippy` then `cargo +<ver> clippy …`) rather than bumping the default.

**`npm run tauri build` requires `TAURI_SIGNING_PRIVATE_KEY`** since v1.0: `bundle.createUpdaterArtifacts` is enabled, so the bundle step fails without the updater signing key. For a local smoke-test without the key use `scripts/build-app.sh --unsigned` (skips updater artifacts and notarization). The full signed/notarized release flow is documented in `docs/RELEASING.md`.

### First `cargo build` is slow

`git2` is built with `vendored-libgit2 + vendored-openssl + https` features, which compiles libgit2 *and* OpenSSL in-tree on the first build. Expect several minutes. Cached after that.

## Architecture

### Two-process model

- **Rust core** (`src-tauri/src/`) — all networking, secrets, filesystem, libgit2.
- **SvelteKit SPA** (`src/`) — static-adapter SPA loaded by Tauri's webview. Two routes: `/` (main window) and `/popover` (menu-bar popover).
- Communication: Tauri commands. Every callable function is registered in `src-tauri/src/lib.rs`'s `invoke_handler![...]` list and called from the frontend via `@tauri-apps/api`'s `invoke('command_name', { args })`. **Adding a backend command requires editing that list** — the registration is the contract.

### Two windows

- `popover` — small (440×620), anchored under the tray icon. Always-on. In release builds it auto-hides on focus-loss (the `WindowEvent::Focused(false)` handler in `lib.rs` is gated by `#[cfg(not(debug_assertions))]` so devtools/screenshots don't dismiss it during dev).
- `main` — full window for repo browsing, settings, account management. Close button hides instead of quits and flips the dock icon off (`ActivationPolicy::Accessory`).

App stays out of the dock by default (`Accessory`); opening `main` flips to `Regular` until it's hidden again.

### Provider modules

`github.rs`, `gitlab.rs`, `codeberg.rs` each own one forge. Since M6.4+ they sit behind the shared `ProviderBackend` trait in `provider_util.rs` (`token`, `base_url`, `list_waiting`, `list_repos`, `list_releases(&[Repo])`, `list_ci(&[Repo])`, `asset_request`); the aggregator and commands hold `Arc<dyn ProviderBackend>` keyed by account id. Construction stays an inherent `connect()` per concrete type. Auth/data commands are provider-generic (`provider_set_token`, `accounts_*`); only the GitHub OAuth device flow keeps `gh_oauth_*` commands. The repo list is fetched once per tick by the aggregator and passed into `list_releases`/`list_ci` — don't re-fetch it inside a provider.

The per-provider `provider_status` / `provider_disconnect` pair was removed in the 2026-07-27 review pass: neither had a UI call site, and `provider_disconnect` swept *every* account sharing a slug (for GitLab that meant every self-hosted instance) in one unconfirmed call. `accounts_list` and `accounts_disconnect` are the per-account replacements.

Provider error handling is centralised in `provider_util::response_error`, which needs the response *headers*, not just the status: HTTP 403 means rate-limit exhaustion when `x-ratelimit-remaining: 0` or `retry-after` is present and an auth/scope failure otherwise. Call sites that treat 403 as a graceful "feature disabled" must guard on `is_rate_limited` first. Per-repo fan-outs (releases + CI) share one `tokio::sync::Semaphore` per provider — see `MAX_CONCURRENT_REPO_CALLS`.

### Auth & secret storage

- **GitHub**: OAuth Device Flow (default) + PAT (fallback). `client_id` is public and lives in `src-tauri/src/oauth.rs::GITHUB_CLIENT_ID`. **Authorization Code + PKCE was explicitly rejected** — see `docs/DECISIONS.md` 2026-05-18 entry. Rotation procedure is documented there too.
- **GitLab, Codeberg/Gitea/Forgejo**: PAT only. OAuth is **not** planned for these — also see DECISIONS.
- **Keychain layout**: one entry per account, keyed `<provider-slug>:<login-lowercase>` (e.g. `github:bjoernw`). PAT entries store the bare token; OAuth entries store a JSON `OAuthTokens` blob. The pre-M6.3 flat-per-provider layout is migrated on first launch by `AppState::ensure_initialized`.
- **Account registry** (non-secret metadata): `accounts.json`, versioned at `1`, written atomically via `util::atomic_write` (same shape as `settings.json`).

### Local index

`local_index.rs` uses `walkdir` to find `.git` directories under configured scan roots, then `git2` to report branch, dirty state, ahead/behind, etc. **No shelling out to `git`** — everything goes through libgit2 bindings. (`downloads.rs` does shell out to `/usr/bin/xattr` to quarantine downloaded release files; the rule is about git.) `list_local_repos` reads the aggregator cache; `rescan_local_repos` walks the disk now — use it when the UI knows the disk just changed.

`remote_fetch.rs` keeps the `behind` counts honest: a separate loop (default 15 min, `remote_fetch_*` settings) libgit2-fetches each clone's upstream — SSH remotes via their HTTPS twin, token only to the matching account's origin — then rescans. Every scan goes through `AppState::store_locals`, which merges per-clone fetch errors and sets the tray count badge; don't write `cache.locals` directly. See the 2026-11-03 entry in `docs/DECISIONS.md`.

### The binary is also a git credential helper

`main.rs` calls `run_cli` before anything of the GUI starts: `gitbuddy credential --account <id> --host <host> get` answers git's credential protocol from the Keychain (`git_credential.rs`) and exits. `clone_repo` (and `enable_git_push` for existing clones) write a URL-scoped `credential.<forge>.helper` entry into the clone's own `.git/config` — an empty reset entry first, then the helper — so `git push` over HTTPS works from any terminal. Never touch `~/.gitconfig`. The helper path is the running binary (`std::env::current_exe`), so a clone made from `npm run tauri dev` points at `src-tauri/target/debug/gitbuddy` — re-run "Enable push via gitBuddy" from the installed app to repoint it. See the 2026-09-25 entries in `docs/DECISIONS.md`; the same day's other entry covers release downloads (`download_release_asset`, token only to the account's own origin via `ProviderBackend::asset_request`).

## macOS-specific quirks

### Ad-hoc codesign wrapper for dev builds

`src-tauri/.cargo/config.toml` wires `cargo run` on macOS through `src-tauri/scripts/sign-and-run.sh`, which re-signs the freshly-built binary with the stable identifier `dev.soron2038.gitbuddy`. Without this, every rebuild gets a fresh transient identifier and macOS invalidates all "Always Allow" Keychain grants → six fresh prompts every relaunch.

If Keychain prompts return, verify the identifier is still stable:

```bash
codesign -d --verbose=4 src-tauri/target/debug/gitbuddy 2>&1 | grep Identifier=
```

This wrapper does **not** run for `tauri build` (release bundles) — production signing is a future milestone via Apple Developer ID.

### Not Mac App Store compatible

`Cargo.toml` enables Tauri's `macos-private-api` feature for real window transparency (rounded popover corners). This uses private Apple symbols, which precludes MAS submission. Distribution is a signed/notarized DMG via GitHub Releases with in-app auto-update (`tauri-plugin-updater`, minisign-signed `latest.json`) — shipping since v1.0; the release procedure lives in `docs/RELEASING.md`.

### Tray icon is an embedded template PNG

`src-tauri/icons/tray-icon.png` is `include_bytes!`'d into the binary and used as a template image (macOS inverts it per system appearance). Regenerate it via the Python script — system SVG-to-PNG converters produce unreliable output for this size.

### The tray count badge is a native view, not part of the icon

The terracotta badge can't live in the template image, and tinting the robot ourselves would fight the wallpaper-driven menu-bar tint. So `tray_badge.rs` swaps in `tray-icon-badged.png` (same robot, wider, with a round gap) and draws the count in an `NSView` subclass on the status item's button; `hitTest:` returns nil so clicks still reach tray-icon's overlay. Its geometry lives in both `tray_badge.rs` and `scripts/regenerate-tray-icon.py`. `objc2`/`objc2-app-kit` must stay on the versions tray-icon uses, because `ns_status_item()` hands us its types. `set_title(None)` is a no-op on macOS, which is why the first `↓N` title badge got stuck. See the 2026-11-04 entry in `docs/DECISIONS.md`.

### Tray menu is attached only while it is shown

On macOS the right-click menu is deliberately **not** passed to `TrayIconBuilder::menu` (Windows does pass it; see below). On macOS 27 a menu that is permanently attached to the `NSStatusItem` swallows the left click (AppKit pops the menu before `tray-icon`'s overlay view sees the event), so `TrayIconEvent::Click` never fires and the popover can't open. `show_tray_menu` in `lib.rs` attaches the menu on right mouse-down, pops it, and detaches it again — the same trick upstream shipped in `tray-icon` 0.25.1, which no Tauri 2.x release pulls in yet. Don't "clean this up" by restoring `.menu(&menu)`; see the 2026-09-16 entry in `docs/DECISIONS.md`.

### Appearance is set natively, the CSS only follows

The Settings → Appearance choice (System / Light / Dark, `settings::Appearance`) is applied by `appearance::apply` in Rust, which calls `WebviewWindow::set_theme` on every window. On macOS that sets `NSApp.appearance`; WKWebView follows it, so `app.css` keeps keying dark mode on `@media (prefers-color-scheme: dark)` — no `data-theme` attribute, no per-component branching. Use the per-window call, **never `AppHandle::set_theme`**: the app-level one leaves each window's cached theme stale, and `apply` reads `main.theme()` for the System case. The native window background can't follow CSS, so `appearance.rs` paints it from `PAPER_LIGHT`/`PAPER_DARK`, which mirror `--paper` in `app.css` — a Rust test fails when they drift. `app.html` declares `color-scheme: light dark` so form controls and scrollbars follow as well. See the 2026-09-29 entry in `docs/DECISIONS.md`.

## Windows-specific quirks

The Windows behaviour is decided in the 2026-11-22 entries of `docs/DECISIONS.md`.

### Tray: menu attached, icons per taskbar theme
- **Menu:** attached for good (`.menu(&menu)` outside macOS). The macOS attach-while-shown trick loses the menu's clicks on Windows, so `show_tray_menu` is macOS-only.
- **Icon:** no template tinting on Windows, so `tray_windows.rs` swaps between four 32×32 PNGs: `tray-win-{light,dark}[-badged].png`, chosen by the *taskbar* theme (`SystemUsesLightTheme`).
- **Badge:** a terracotta dot; the count is in the tooltip.
- **Main thread:** every swap goes through `run_on_main_thread`, because `set_icon` blocks off the main thread and the swap holds a lock.
- **Regenerating:** the PNGs come from `scripts/regenerate-tray-icon.py`, and it leaves the macOS PNGs pixel-identical.

### Popover placement and the focus race
- **Placement:** `popover.rs` holds pure, tested geometry. The popover opens away from the taskbar edge, found from the work-area inset or, with an auto-hiding taskbar, the icon. It is clamped into the work area of the icon's monitor at that monitor's scale. macOS keeps its inline formula in `lib.rs`.
- **Focus race:** clicking the tray focuses the taskbar on mouse-down, so the popover hid itself and reopened on mouse-up. `POPOVER_AUTO_HIDDEN_AT` + `popover::REOPEN_GUARD` (500 ms) keep it closed. Auto-hide exists only in release builds, so test toggling with one.

### Lifecycle
- **Single instance:** `tauri-plugin-single-instance` is the first plugin, Windows only. It is pinned to `~2.4.5` because 2.5 needs tauri 2.12, and the crate stays on 2.11 (2026-09-16).
- **Autostart:** the autostart Run key passes `--autostart`. Every other launch opens the main window, since a new tray icon hides in the overflow.

### Secrets, helper, downloads
- **Token store:** Credential Manager via `keyring`'s `windows-native`. Without a store feature, keyring silently uses an in-memory mock.
- **Helper path:** written with `/` (`git_credential::shell_path`) for Git for Windows' MSYS `sh`.
- **Downloads:** they get the Mark-of-the-Web (`<file>:Zone.Identifier`, `ZoneId=3`) instead of the xattr quarantine flag. It never contains the token.

### Editor and terminal
- **Launching:** `launcher.rs` parses the command line and resolves it with the `which` crate (PATHEXT: `code` → `code.cmd`). It spawns without a shell; `.cmd` runs with `CREATE_NO_WINDOW`.
- **Terminal presets:** `wt`, Git Bash; anything else gets its own console.
- **macOS:** keeps `open -a` (`cfg(not(windows))` in `commands.rs`).
- **Testing:** a debug build passes its `tauri dev` pipes on to a launched shell, so `cmd` exits at once. Test terminal launches with a release build or the ignored smoke test.

### Paths and names
- **One set of name rules:** `util::file_name_problem` / `sanitize_file_name` apply the Windows rules on every platform (forbidden characters, trailing dot or space, `CON`/`NUL`/`COM1`…).
  - Downloads are rewritten to pass.
  - Clone folder names are refused instead.
- **Scan skips:** `local_index::SKIP_DIRS` includes `AppData`, `$Recycle.Bin` and `System Volume Information`. It compares case-insensitively on Windows only.
- **Line endings:** `.gitattributes` forces LF checkouts. Git for Windows defaults to CRLF, which broke the contrast test's shebang and would break the shell scripts.

### Developing on the Windows PC
- **Build tools:** the MSVC toolchain, plus Python + Pillow for the icon script.
- **PowerShell 5.1 pitfalls:** it drops empty arguments to native programs (`git config --add key ''`), and its pipe into `git` is unreliable. Use Git Bash for those.
- **One instance:** a running gitBuddy (installed or release build) swallows a second launch of the dev build. Quit it first.
- **Claude desktop is an MSIX package.** Anything a Claude Code session starts (installers, `npm run tauri dev`, a release exe) inherits that. Its writes to `%LOCALAPPDATA%` / `%APPDATA%` are redirected into `%LOCALAPPDATA%\Packages\Claude_*\LocalCache\…`, and an empty folder of the same name there hides the real one from the session.
  - An installer run that way installs where the user can't see it.
  - Install builds by double-clicking them in Explorer, and read the real files only after removing such leftovers from `LocalCache`.
  - Credential Manager, Documents and Downloads are not affected.

## Conventions to follow

- **`docs/DECISIONS.md` is append-only**. If a decision is being reversed, add a new dated entry that explains why and points back at the older one — don't edit history.
- **Rust unit tests live in per-module `#[cfg(test)]` blocks** (~200 tests, a few of them Windows- or macOS-only: provider fixture deserialization, oauth parsing, settings migrations, aggregator diff logic, libgit2 tests against temp fixture repos in `local_index.rs`, and a credential-helper test that drives a real `git credential fill` when a `git` binary is present). Run via `cargo test --lib`. Includes an HTTP-level **provider conformance suite** (`wiremock` dev-dependency): each provider is pointed at a localhost mock server via the per-provider `#[cfg(test)] for_test` seam and driven through real reqwest requests — pagination, the bearer header, and the rate-limit/401/403/5xx/graceful-404 mappings. Shared test helpers (a `Viewer` stub, a pagination-page generator) live in `provider_util::test_support`. GitHub's API base is a struct field (defaulting to `API_BASE`) purely so this seam can redirect it; `base_url()` still returns `None`.
- **Frontend tests are Vitest, in `src/**/*.test.ts`** next to the module they cover (`format.test.ts`, `data/api.test.ts`, `data/auth.test.ts` — ~44 tests over the pure logic both windows share: key derivation, dedup, formatters, provider resolution, auth-head derivation), plus `app-contrast.test.ts`, which runs `scripts/check-contrast.mjs` over both schemes of `src/app.css`. `vitest.config.ts` deliberately does *not* load the SvelteKit plugin — these are plain functions, so a Node environment plus the `$lib` alias keeps the suite at ~100 ms. Component tests would need the plugin plus jsdom; add a second project there if that day comes.
- **The account migrations take a `keychain::KeychainStore`**, not the free functions, so `migrate_ids` can run against `keychain::test_support::FakeKeychain` (in-memory, with per-key load/save failure injection and a record of every delete). This is the code that can lose a user's stored token — the partial-failure branches are covered, in particular that a failed copy never deletes the v1 entry.
- **Settings & accounts use `util::atomic_write`** — never write JSON config files directly with `serde_json::to_writer`; the atomic helper survives mid-write crashes.
- **Palette colours are computed, not eyeballed.** Every token change in `src/app.css` has to clear the contrast floors in `scripts/check-contrast.mjs` (body ink ≥ 15:1, small text and each `--*-ink` on its `--*-soft` ≥ 4.5:1, …); `npm run test` enforces it. Text on an accent's soft tint uses that accent's `--*-ink` token, never the accent itself. `color-mix()` is off-limits (macOS 11 support), which is why hover shades and the scheme-dependent surfaces (`--raised`, `--knob`, `--edge`, `--scroll-thumb`) are tokens.
- **Frontend uses Svelte 5 runes** (`$state`, `$derived`, etc.) — not the legacy reactive `$:` syntax.
- **Static adapter, no SSR**: `svelte.config.js` uses `@sveltejs/adapter-static`. The frontend is a single-page bundle Tauri loads from disk; there is no server runtime.
