# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

gitBuddy is a **macOS menu-bar companion** for GitHub, GitLab, and Codeberg/Gitea/Forgejo. It aggregates repos, issues/PRs, releases, and CI status across multiple forge accounts, and surfaces the state of local clones (via libgit2). Tauri 2 shell + SvelteKit 2 / Svelte 5 frontend + Rust core.

Scope and milestones live in `PRD.md`. Architecture decisions with dated rationales live in `docs/DECISIONS.md` (append-only — never edit past entries; add a new entry that references the older one if reversing course).

## Commands

All commands run from repo root unless noted.

| Task | Command |
|------|---------|
| Dev (Tauri + Vite) | `npm run tauri dev` |
| Frontend type-check | `npm run check` |
| Frontend type-check (watch) | `npm run check:watch` |
| Rust check | `cd src-tauri && cargo check --all-targets` |
| Rust lint (CI gate) | `cd src-tauri && cargo clippy --all-targets -- -D warnings` |
| Rust format check (CI gate) | `cd src-tauri && cargo fmt --all -- --check` |
| Rust format apply | `cd src-tauri && cargo fmt --all` |
| Release bundle (unsigned, local) | `npm run tauri build` |
| Tray icon regen | `python3 scripts/regenerate-tray-icon.py` |

CI runs all four check/lint commands plus an unsigned Tauri bundle on `macos-14`. Anything that fails CI fails locally with the same command — there is no Linux/Windows runner.

### First `cargo build` is slow

`git2` is built with `vendored-libgit2 + vendored-openssl + https` features, which compiles libgit2 *and* OpenSSL in-tree on the first build. Expect several minutes. Cached after that.

## Architecture

### Two-process model

- **Rust core** (`src-tauri/src/`) — all networking, secrets, filesystem, libgit2.
- **SvelteKit SPA** (`src/`) — static-adapter SPA loaded by Tauri's webview. Two routes: `/` (main window) and `/popover` (menu-bar popover).
- Communication: Tauri commands. Every callable function is registered in `src-tauri/src/lib.rs`'s `invoke_handler![...]` list and called from the frontend via `@tauri-apps/api`'s `invoke('command_name', { args })`. **Adding a backend command requires editing that list** — the registration is the contract.

### Two windows

- `popover` — small (~360×500), anchored under the tray icon. Always-on. In release builds it auto-hides on focus-loss (the `WindowEvent::Focused(false)` handler in `lib.rs` is gated by `#[cfg(not(debug_assertions))]` so devtools/screenshots don't dismiss it during dev).
- `main` — full window for repo browsing, settings, account management. Close button hides instead of quits and flips the dock icon off (`ActivationPolicy::Accessory`).

App stays out of the dock by default (`Accessory`); opening `main` flips to `Regular` until it's hidden again.

### Provider modules

`github.rs`, `gitlab.rs`, `codeberg.rs` each own one forge. They are **not** behind a shared trait yet (PRD §6.2 calls for one, but as of M6.4 each provider's `commands::*` entry points are explicit per-provider: `gh_*`, `gl_*`, `cb_*`). When generalising, the trait should expose `list_repos`, `list_items`, `list_releases`, `list_ci_runs`, `authenticate`.

### Auth & secret storage

- **GitHub**: OAuth Device Flow (default) + PAT (fallback). `client_id` is public and lives in `src-tauri/src/oauth.rs::GITHUB_CLIENT_ID`. **Authorization Code + PKCE was explicitly rejected** — see `docs/DECISIONS.md` 2026-05-18 entry. Rotation procedure is documented there too.
- **GitLab, Codeberg/Gitea/Forgejo**: PAT only. OAuth is **not** planned for these — also see DECISIONS.
- **Keychain layout**: one entry per account, keyed `<provider-slug>:<login-lowercase>` (e.g. `github:bjoernw`). PAT entries store the bare token; OAuth entries store a JSON `OAuthTokens` blob. The pre-M6.3 flat-per-provider layout is migrated on first launch by `AppState::ensure_initialized`.
- **Account registry** (non-secret metadata): `accounts.json`, versioned at `1`, written atomically via `util::atomic_write` (same shape as `settings.json`).

### Local index

`local_index.rs` uses `walkdir` to find `.git` directories under configured scan roots, then `git2` to report branch, dirty state, ahead/behind, etc. **No shelling out to `git`** — everything goes through libgit2 bindings.

## macOS-specific quirks

### Ad-hoc codesign wrapper for dev builds

`src-tauri/.cargo/config.toml` wires `cargo run` on macOS through `src-tauri/scripts/sign-and-run.sh`, which re-signs the freshly-built binary with the stable identifier `dev.soron2038.gitbuddy`. Without this, every rebuild gets a fresh transient identifier and macOS invalidates all "Always Allow" Keychain grants → six fresh prompts every relaunch.

If Keychain prompts return, verify the identifier is still stable:

```bash
codesign -d --verbose=4 src-tauri/target/debug/gitbuddy 2>&1 | grep Identifier=
```

This wrapper does **not** run for `tauri build` (release bundles) — production signing is a future milestone via Apple Developer ID.

### Not Mac App Store compatible

`Cargo.toml` enables Tauri's `macos-private-api` feature for real window transparency (rounded popover corners). This uses private Apple symbols, which precludes MAS submission. Distribution is signed/notarized DMG via GitHub Releases (post-M7).

### Tray icon is an embedded template PNG

`src-tauri/icons/tray-icon.png` is `include_bytes!`'d into the binary and used as a template image (macOS inverts it per system appearance). Regenerate it via the Python script — system SVG-to-PNG converters produce unreliable output for this size.

## Conventions to follow

- **`docs/DECISIONS.md` is append-only**. If a decision is being reversed, add a new dated entry that explains why and points back at the older one — don't edit history.
- **No test infrastructure exists yet** on the Rust side. When adding tests, the PRD's verification plan (§12) calls for provider conformance tests with recorded HTTP fixtures, libgit2 wrapper tests against fixture repos, and aggregator merge tests.
- **Settings & accounts use `util::atomic_write`** — never write JSON config files directly with `serde_json::to_writer`; the atomic helper survives mid-write crashes.
- **Frontend uses Svelte 5 runes** (`$state`, `$derived`, etc.) — not the legacy reactive `$:` syntax.
- **Static adapter, no SSR**: `svelte.config.js` uses `@sveltejs/adapter-static`. The frontend is a single-page bundle Tauri loads from disk; there is no server runtime.
