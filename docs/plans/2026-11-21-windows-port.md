# Windows port (x64) — effort estimate and implementation plan

> **Status:** drafted 2026-11-21; execution started the same day on
> `feat/windows-port` (WS1 + WS2). Progress is tracked in the table below —
> update it as work packages land, since the port continues across machines.
> Facts and line numbers are from the 1.6.0 tree; re-check them before editing.

## Context

gitBuddy is a macOS-only menu-bar app today (Tauri 2.11, Rust core, Svelte 5
SPA). The PRD has named portability to Windows/Linux as an architecture goal
since day one and lists "Windows and Linux release builds (architecture is
ready; needs CI + signing)" as out of scope for v1. This plan covers a **full
Windows release (x64)**: signed installer, auto-update through the same
`latest.json`, and a Windows CI gate.

**Decisions taken (2026-11-21):**
- A real Windows x64 PC is available for interactive testing.
- Windows installers are built in **GitHub Actions**.
- Authenticode signing via **SignPath Foundation** (free for OSS; the repo is
  MIT-licensed and public).
- The macOS release stays local and notarised (`scripts/release.sh`).

## Summary

- **Effort: about 16–20 person-days** (3–4 focused weeks). **Calendar time is
  set by SignPath's approval**, not by the code — that can take weeks and is
  not plannable.
- **Good starting point:** the Rust core already compiles on Windows — all
  objc2/AppKit code is `cfg`-gated. libgit2 uses WinHTTP on Windows (no
  OpenSSL, Perl or NASM). Every Tauri plugin in use is cross-platform,
  `icon.ico` exists, and the CSP already allows `ipc.localhost` /
  `asset.localhost`.
- **Where the work is:**
  1. Tray, popover and window lifecycle behave differently on Windows.
  2. Secret storage: `keyring` is built with `apple-native` only, so on Windows
     it **silently uses an in-memory mock store** — accounts vanish on restart.
  3. "Open in editor/terminal" hard-codes `/usr/bin/open -a`.
  4. The release pipeline with SignPath (two signing passes plus a local
     updater signature).
- **Caveat on "full release in one go":** SignPath's terms require the project
  to be "already released in the form that should be signed". Realistically the
  **first Windows build ships unsigned as a "Windows preview"**, and signing
  arrives with the next release once SignPath has approved. The code is still
  one pass; only the signing state comes in two steps.

## Progress

| WS | Package | Status |
|---|---|---|
| WS0 | SignPath application (maintainer action) | open |
| WS1 | Windows CI gate + the fixes it forces | done locally (clippy + 173 tests green on the PC); **open:** first `backend-windows` CI run |
| WS2 | Secret storage (Credential Manager) | done: the round-trip test passes against the real Credential Manager |
| WS3 | Config and bundling | done on Windows: CLI 2.12.6, flags present, unsigned NSIS build. **Open:** `build-app.sh --unsigned` on macOS |
| WS4 | Tray, popover, lifecycle | done on Windows, tested on the PC (see below). **Open:** the WS10 cases the PC can't show, the macOS regression check |
| WS5 | External programs + credential helper | done, tested on the PC (see below). **Open:** `code`, `pwsh` and a real `git push` in WS10 |
| WS6 | Filesystem hardening | done, tested on the PC (see below). **Open:** a real release download and SmartScreen in WS10 |
| WS7 | Frontend | type-checked and tested (`npm run verify` green on the PC). **Open:** visual check on macOS |
| WS8 | Release pipeline | written; the unsigned path runs on the PR. **Open:** the SignPath steps (need WS0), a real `release.sh` run on the Mac |
| WS9 | Docs | open (last) |
| WS10 | End-to-end on the Windows PC | open (last) |

**WS8 (2026-11-22), as built:**
- `.github/workflows/release-windows.yml`:
  - Triggers: `v*` tags, `workflow_dispatch` with `signing-policy`, and
    `pull_request` when the workflow file itself changes. That last one is
    always unsigned and has no secrets; it lets the pipeline test itself on
    a PR, since `workflow_dispatch` only exists once the file is on `main`.
  - Policy: the dispatch input, otherwise the repo variable
    `WINDOWS_SIGNING_POLICY`, otherwise `none`. Turning on signing after
    SignPath's approval is just setting that variable to `release-signing`.
  - The job uses the `release` environment only when it signs: an empty
    environment name means none. No caches.
  - Steps: check that the tag matches `tauri.conf.json`, `tauri build
    --no-bundle`, then SignPath (action v2, pinned by commit; artifact config
    `tauri-app-exe`), then `scripts/check-authenticode.ps1`, then `tauri
    bundle --bundles nsis --no-binary-patching` without updater artifacts,
    then SignPath again (`tauri-installer`) and the check.
  - The artifact `windows-installer` holds the setup exe, `SHA256SUMS` (LF,
    the format `shasum -c` reads) and `SIGNING_POLICY`.
- `scripts/check-authenticode.ps1`: for release-signing the signature must
  be valid; for test-signing any signature is enough. A timestamp is always
  required.
- `scripts/generate-latest-json.sh`:
  - It maps `release/*_x64-setup.exe` to `windows-x86_64`, the plain key,
    because without binary patching the app can't name its installer type.
  - It refuses when any of darwin-aarch64, darwin-x86_64 or windows-x86_64
    is missing, or when an installer is stale.
  - `--no-windows` leaves Windows out deliberately.
  - Tested in Git Bash with fake artifacts: all three present, a missing
    installer, a missing `.sig`, a stale installer, `--no-windows`.
- `scripts/release.sh`:
  - New options `--windows-run <id>` and `--no-windows`.
  - After the credential checks it asks, pushes the tag and finds the
    workflow run. The `jq` lookup was tested against real runs.
  - After the Mac build it runs `gh run watch` and `gh run download`, checks
    `shasum -c`, copies the installer to `release/` with its `.policy`, and
    runs `tauri signer sign --app-version` (an empty passphrase is passed
    explicitly).
  - An unsigned installer puts a "Windows preview" note at the top of the
    release notes; a test-signed one is refused.
  - It creates a draft with all assets, compares the asset list, then runs
    `gh release edit --draft=false --latest`.
  - The stale sweep keeps `gitBuddy_<version>_x64-setup.exe*`.
  - `--dry-run` without `--windows-run` pushes no tag and leaves Windows
    out.
  - Only `bash -n`, `--help` and the argument checks could run on the PC;
    the rest needs the Mac.
- RELEASING.md isn't updated yet (WS9).

**WS6 (2026-11-22), as built:**
- `util.rs` has the shared name rules, the same on every platform:
  - `file_name_problem` covers forbidden `<>:"/\|?*`, control characters, a
    trailing dot or space, and reserved device names (`CON`, `PRN`, `AUX`,
    `NUL`, `COM0–9`, `LPT0–9`, also `COM¹–³`/`LPT¹–³`, any case, any
    extension, `NUL .txt` too).
  - `sanitize_file_name` rewrites a name to pass those rules: `_` for bad
    characters, trailing dots and spaces stripped, a `_` prefix for device
    names.
- `downloads.rs`:
  - `safe_file_name` uses `sanitize_file_name`.
  - On Windows, `mark_quarantined` writes the Mark-of-the-Web
    (`<file>:Zone.Identifier`, `ZoneId=3`). Its `HostUrl` is the asset's
    browser URL without query, fragment or user info, never the token.
  - `fetch_to` takes that URL as a new parameter.
- `clone_repo`: the folder name goes through `clone_folder_name`, which
  rejects instead of rewriting. It must pass `file_name_problem` and be
  exactly one `Component::Normal`. `D:evil`, `..`, `CON`, `a/b` are refused.
  - This also applies on macOS: a repo cloned into a folder named `aux` now
    has to be renamed.
- `local_index.rs`:
  - `SKIP_DIRS` adds `AppData`, `$Recycle.Bin` and
    `System Volume Information`. OneDrive is not skipped.
  - `is_skipped` ignores case on Windows only; on macOS a `library` folder
    of repos stays visible.

Checked on the PC: 200 lib tests, including a fetch into a temp folder on
NTFS whose `Zone.Identifier` holds `ZoneId=3` and the `HostUrl`, without the
token, and a scan that skips `appdata\…` but finds `Projects\…`.

Not covered yet (WS10): a real release download, SmartScreen's warning on
launching it, and a clone into a folder with spaces from the UI.

**WS5 (2026-11-22), as built:**
- New `launcher.rs`, Windows only at runtime. The parsing is pure and tested
  on every platform:
  - `parse_command_line`: a leading `"…"` is the program, an unquoted path to
    an existing file is the program, otherwise the line is split on
    whitespace.
  - `classify_terminal`: `wt`/`Windows Terminal` → `wt -d` (`;` escaped as
    `\;`); `Git Bash`/`git-bash` → `git-bash.exe --cd=` (found beside `git`
    or under `%ProgramFiles%\Git`); anything else → started in the folder
    with `CREATE_NEW_CONSOLE`.
  - Programs resolve through the `which` crate (PATHEXT). `.cmd`/`.bat` get
    `CREATE_NO_WINDOW`.
- macOS: `run_editor`/`run_terminal` behave exactly as before. The `.app`
  logic and its tests are now `cfg(not(windows))`.
- `settings.rs`: the scan-root candidates add `source\repos` and
  `Documents\GitHub` on Windows. The pick is testable through
  `default_scan_roots_in`.
- `git_credential.rs`:
  - `unusable_exe` refuses an exe under `%TEMP%`, such as Explorer's copy of
    an exe opened inside a zip.
  - The docs say that the empty `helper =` reset also silences Git
    Credential Manager, on purpose.

Checked on the PC:
- Risk 2: the release exe (GUI subsystem) works as the credential helper.
  `git credential fill` from Git Bash, cmd and PowerShell returned the test
  entry from Credential Manager in about 160 ms with no console window, and
  stayed silent for other hosts. The test entry was deleted afterwards.
- `launcher::tests::smoke_opens_the_configured_programs` (ignored; set
  `GITBUDDY_SMOKE_EDITOR` / `GITBUDDY_SMOKE_TERMINALS` and run with
  `--ignored`):
  - `wt`, `powershell` and `Git Bash` opened in the repository.
  - The quoted `…\Antigravity IDE\bin\antigravity-ide.cmd` opened the
    folder, with no leftover console.
- From a GUI-subsystem parent, `cmd` and `powershell` stay open in their new
  console. With Windows Terminal as the default terminal they open as tabs
  there.
- Caveat: a debug build is a console program and passes the `tauri dev`
  pipes on to the shell, so `cmd` exits at once. Test terminal launches with
  a release build.

Not covered yet (WS10): `code`, `pwsh`, `git push` with a real account,
clicking through the right-click menu of a listed clone (needs a connected
account).

**WS4 (2026-11-22), as built:**
- Tray menu: attached for good outside macOS; `show_tray_menu` is macOS-only.
- Placement: `popover.rs` holds the pure, tested geometry (`taskbar_edge`,
  `popover_origin`, 13 tests). `lib.rs::popover_position` picks the monitor
  and sizes the popover at that monitor's scale. If the scale changes on
  `show`, it places the popover a second time. Gap and margin are 12 logical
  px. macOS keeps its inline formula unchanged.
- Focus race: the focus-lost handler records when it hid a *visible* popover.
  A tray click within 500 ms of that leaves it closed
  (`popover::REOPEN_GUARD`). This applies on macOS too, where it only matters
  for a reopen within 500 ms of an outside click.
- Tray icon:
  - `tray_windows.rs` swaps between four 32×32 PNGs (light/dark taskbar, with
    and without the terracotta dot), chosen by `SystemUsesLightTheme`.
  - Every swap runs on the main thread. `set_icon` blocks off it, and a lock
    held across that call would deadlock against a tray click.
  - The theme is re-read on every scan, every tray click and every
    `ThemeChanged`.
  - The script now draws the robot with a view and a colour as parameters.
    The macOS PNGs came out pixel-identical.
- Single instance: `tauri-plugin-single-instance` `~2.4.5`, Windows-only. 2.5
  needs tauri ^2.12.
- Autostart: the Run key passes `--autostart`; every other launch opens the
  main window.

Checked on the PC (one monitor, auto-hide taskbar at the bottom, dark theme,
icon in the overflow):
- In the dev build: the main window opens on a normal launch, and a second
  launch exits and shows the running app's main window. A second launch with
  `--autostart` leaves it closed.
- The popover opens centred above the overflow icon.
- The right-click menu shows, and "Open gitBuddy" works.
- In a release build, a second tray click closes the popover for good.

Not covered yet (WS10):
- The other taskbar edges, a second monitor, mixed DPI.
- A light taskbar, and the badge dot with clones that are behind.
- Closing the popover by clicking outside it, then reopening.
- "Quit" from the menu.
- Sign-in autostart.
- On macOS: the popover position, the menu on macOS 27 and the badge.

Python 3.13 and Pillow are now installed on the PC (`winget`, `pip --user`),
so the tray icons can be regenerated there.

**Pick up here (2026-11-22, morning):** the 2026-11-21 list below was worked
through on the Windows PC (Windows 11, MSVC, Rust 1.99.1, Node 26):
- `cargo fmt` reflowed one test. clippy `-D warnings` is clean, and all 173 lib
  tests pass, including the Credential Manager round trip.
- `Cargo.lock` gained keyring's `windows-native` dependencies.
- `npm run verify` is green: svelte-check reports 0 errors, Vitest 69 passed.
- `@tauri-apps/cli` is at 2.12.6. `bundle --no-binary-patching` and
  `signer sign --app-version` exist.
- An unsigned `tauri build --bundles nsis` (updater artifacts off) produced
  `gitBuddy_1.6.0_x64-setup.exe` without a version-mismatch warning.
- New `.gitattributes` (`* text=auto eol=lf`). With Git for Windows' default
  `core.autocrlf=true`, the `#!…\r` shebang of `scripts/check-contrast.mjs`
  broke `app-contrast.test.ts` under Vitest. A clone made **before** this
  change keeps its CRLF files until they are checked out again.

Still open from that list:
- On macOS, `scripts/build-app.sh --unsigned`.
- On macOS, a visual check that the main window and popover look as before.
- The first run of the `backend-windows` CI job, on the draft PR.

WS4, WS5, WS6 and WS8 followed the same day (above). Left: WS0 (SignPath,
the maintainer's application), WS9 (docs, including RELEASING.md for the
new flow), WS10 (end-to-end, which needs a connected account on the PC),
and a first real `release.sh` run on the Mac.

**Pick-up list of 2026-11-21:** WS1, WS2, WS3 (partly) and WS7 were written on
a Mac with neither Rust nor Node, and merged into `feat/windows-port`. Nothing
has been compiled, formatted, type-checked or tested yet. On the next machine:
1. In `src-tauri/`: `cargo fmt --all`, `cargo clippy --all-targets -- -D
   warnings`, `cargo test --lib`. The first build also refreshes `Cargo.lock`
   for keyring's `windows-native` dependencies; commit the lockfile.
2. From the repo root: `npm ci`, then `npm run verify`. Check that the macOS
   main window and popover render and read exactly as before (WS7 keeps the
   macOS strings byte for byte).
3. Finish WS3:
   - Bump `@tauri-apps/cli` to ≥ 2.12 through npm, so the lockfile follows.
     Don't hand-edit `package.json`, or `npm ci` fails on the mismatch.
   - Confirm that `npm run tauri -- bundle --help` lists
     `--no-binary-patching`, and that `signer sign --help` lists
     `--app-version`.
   - Check the CLI doesn't warn about a version mismatch with the `tauri`
     2.11 crate.
   - Run `scripts/build-app.sh --unsigned` on macOS.
4. Open a (draft) PR to `main` so the new `backend-windows` job runs. That job
   is the only place the Windows side of the `cfg` gates compiles before WS10.
   Expect a few more Windows-only clippy or test findings on its first run.

What WS1/WS2 changed:
- `lib.rs`: the `app_handle` clone is macOS-only (it was an unused variable on
  Windows). `show_tray_menu` is deliberately still ungated; WS4 gates it once
  Windows attaches the menu natively.
- `remote_fetch.rs`: `is_local_path` lets drive-letter and UNC remotes through
  `fetch_url`. Tests: `fetch_url_passes_https_and_paths_through` gained the
  Windows forms, plus the new `a_drive_letter_is_not_an_ssh_host`.
- `git_credential.rs`: `shell_path` writes the helper's exe path with `/` on
  Windows; the git-driven test uses it for its global helper too. New test:
  `windows_paths_are_spelled_with_slashes_for_the_shell`.
- `util.rs`: `rename_replacing` retries a `PermissionDenied` rename up to five
  times on Windows (about 310 ms in all).
- `ci.yml`: new `backend-windows` job running clippy and tests (no fmt, no
  bundle).
- `Cargo.toml`: keyring now has `windows-native`. `keychain.rs` docs updated,
  plus a CI-only Windows test that saves and reads back a credential.
  `oauth.rs` has a size test for the worst-case token blob.

## Work packages (recommended order)

### WS0 · SignPath application (day 0, because of the wait) · 0.5–1 PD
- Apply at signpath.org/apply; enable 2FA on GitHub and SignPath.
- README: a "Code signing policy" section with the required sentence ("Free
  code signing provided by SignPath.io, certificate by SignPath Foundation")
  and the roles (committers/reviewers, approvers — all Soron2038).
- New `PRIVACY.md`. The app opens connections on its own: the update check to
  github.com, and fonts from api.fontshare.com and fonts.googleapis.com
  (`src/app.html:10–20`).
  - SignPath asks for disclosure and an opt-out.
  - Recommendation: **self-host the fonts** (also helps offline) and add an
    "Automatically check for updates" setting — a `#[serde(default)]` field,
    following the pattern in `settings.rs`.
- After approval:
  - SignPath project with origin verification.
  - Policies `test-signing` and `release-signing`.
  - Artifact configurations `tauri-app-exe` and `tauri-installer` (a
    `zip-file` wrapping a `pe-file`), with copies in
    `.signpath/artifact-configurations/`.
  - GitHub environment `release` (only `v*` tags) holding `SIGNPATH_API_TOKEN`;
    repo variable `SIGNPATH_ORGANIZATION_ID`.

### WS1 · Windows CI gate and the fixes it forces · 1.5 PD
- `.github/workflows/ci.yml`: new job `backend-windows` on `windows-latest`
  with rust-cache, `cargo clippy --all-targets -- -D warnings` and
  `cargo test --lib`. fmt stays on macOS; the "Force cargo" step stays
  macOS-only.
- `src-tauri/src/lib.rs:199`: move the `app_handle` clone into the macOS `cfg`
  block (unused variable → clippy fails on Windows). Gate `show_tray_menu` with
  `#[cfg(target_os = "macos")]` once WS4 attaches the menu natively on Windows.
- `remote_fetch.rs::fetch_url` (336–341): pass absolute Windows paths (`C:\`,
  `C:/`, UNC) through via `Path::is_absolute()`, **before**
  `parse_remote_url` (which would read `C:\x` as an SCP-style remote with host
  `C`). Fixes a real runtime bug (local remotes silently skipped) and four
  tests.
- `git_credential.rs::helper_command` (212): write the exe path with `/` on
  Windows. Same in the test at ~546 — a backslash inside a double-quoted
  gitconfig value is read as an escape sequence.
- `util.rs::atomic_write`: on Windows retry the rename on `PermissionDenied`
  (5 attempts, short backoff). Antivirus and concurrent replaces cause brief
  access-denied errors; this also removes the flake in the concurrency test.

### WS2 · Secret storage (Credential Manager) · 0.5 PD
- `Cargo.toml`: `keyring` with features `["apple-native", "windows-native"]`.
- `keychain.rs`: update the docs; add a test that the largest serialised
  `OAuthTokens` (~711 chars) stays under Credential Manager's limit of 1280
  UTF-16 code units (2560-byte blob).
- Optional `#[cfg(windows)]` CI test: save, then read back through a fresh
  `Entry` — it fails if keyring silently falls back to the mock store.

### WS3 · Config and bundling · 0.5–1 PD
- New `src-tauri/tauri.windows.conf.json`, merged by Tauri as an RFC 7396 JSON
  Merge Patch over the base config:
  ```json
  { "bundle": { "targets": ["nsis"],
      "windows": { "webviewInstallMode": { "type": "downloadBootstrapper" },
                   "nsis": { "installMode": "currentUser" } } },
    "plugins": { "updater": { "windows": { "installMode": "passive" } } } }
  ```
- The macOS keys (`titleBarStyle`, `hiddenTitle`, `acceptFirstMouse`,
  `macOSPrivateApi`) **stay in the base config**: Windows ignores them, and
  `app.windows` is an array that a merge would replace whole.
- Set `bundle.publisher`.
- Bump `@tauri-apps/cli` to **≥ 2.12** (locked at 2.11.1). Only 2.12 has
  `tauri bundle --no-binary-patching`; without it bundling rewrites bytes in
  the already-signed exe and breaks its signature.
- Keep the `tauri` crate at 2.11.x for now: a bump may pull tray-icon 0.25,
  which already contains the macOS 27 fix and would collide with our
  workaround (`show_tray_menu`, DECISIONS 2026-09-16).
- Document the local unsigned build for the PC:
  `npm run tauri build -- --bundles nsis --config '{"bundle":{"createUpdaterArtifacts":false}}'`
  (the same override `build-app.sh --unsigned` uses).

### WS4 · Tray, popover, lifecycle · 3–4 PD (largest code chunk)
- **Context menu** (`lib.rs:115–153, 276–281`): on Windows attach the menu
  permanently via `.menu(&menu)`. The attach/detach trick loses menu clicks
  there: tray-icon's Windows `show_menu` calls `TrackPopupMenu` without
  `TPM_RETURNCMD`, and muda's subclass is already gone when the `WM_COMMAND`
  arrives. The trick stays macOS-only. With a menu attached, tray-icon opens it
  on `WM_RBUTTONUP` and still emits left `Click` events.
- **Popover placement** (`toggle_popover`, 283–308): today it always lands
  *below* the icon — off-screen with a bottom taskbar.
  - New pure, unit-tested `popover_origin(tray, pop_size, monitor, work_area,
    gap)`: derive the taskbar edge from how `Monitor::work_area()` is inset
    from the monitor bounds; with an auto-hide taskbar use the monitor edge
    nearest the tray rect.
  - Place the popover at that edge and clamp it into the work area.
  - Pick the monitor with `monitor_from_point` at the tray centre (fallbacks:
    first of `available_monitors` containing it, then `primary_monitor`).
    Compute the size with the *target* monitor's scale factor (mixed DPI).
  - On macOS the function returns what it returns today.
- **Focus race** (169–177): clicking the tray first gives the taskbar focus →
  popover hides → click-up re-shows it. The focus-lost handler records when it
  hid the popover; `toggle_popover` does not re-show within ~300 ms.
- **Tray icon:**
  - Extend `scripts/regenerate-tray-icon.py` with 32×32 variants for light and
    dark taskbars plus badged variants with a terracotta dot. The count stays in
    the tooltip — digits are unreadable at 16 px.
  - Choose by the registry value `SystemUsesLightTheme`, via `winreg` 0.55
    (already in `Cargo.lock`) as a Windows-only dependency.
  - `set_tray_badge` (233–245) re-checks on every tick and on `ThemeChanged`;
    call `set_icon` only when the state changes.
- **Single instance:** register `tauri-plugin-single-instance` as the **first**
  plugin; its callback calls `open_main_window`. `run_cli` runs before the
  builder, so the credential helper is unaffected.
- **First launch:** Windows 11 hides new tray icons in the overflow. Give the
  autostart plugin `--autostart` as an argument on Windows; when the argument is
  absent at startup, open the main window.

### WS5 · External programs and the credential helper · 1.5–2 PD
- **`commands.rs::run_editor`** (1386–1416):
  - New pure, tested parser: a leading `"…"` is the program; a whole string
    that exists as a file is the program even with spaces; otherwise split on
    whitespace.
  - Resolve with the `which` crate (honours PATHEXT — Rust's `Command` only
    appends `.exe`, so `code` never finds `code.cmd`).
  - Spawn directly, never through a shell; `CREATE_NO_WINDOW` for `.cmd`/`.bat`
    (Rust ≥ 1.77.2 escapes batch arguments safely or refuses with
    `InvalidInput`).
  - `open_app` and the `.app` logic become macOS-only.
- **`run_terminal`** (1426–1447), Windows presets:
  - `wt` → `wt.exe -d <path>` (escape `;`, Windows Terminal's separator)
  - `pwsh`, `powershell`, `cmd` → `current_dir(path)` with `CREATE_NEW_CONSOLE`
  - Git Bash → `git-bash.exe --cd=<path>`
  - anything else → the resolved program with `current_dir`
- **`settings.rs::default_scan_roots`** (227): on Windows also `source\repos`
  and `Documents\GitHub`.
- **`git_credential.rs`:** `unusable_exe` gets a Windows case (exe running from
  `%TEMP%`). Document that the empty `helper =` reset also silences Git
  Credential Manager, which is intended.

### WS6 · Filesystem hardening · 1 PD
- One shared file-name check, active on all platforms (same tests on both
  runners):
  - replace `<>:"/\|?*` and control characters
  - strip trailing dots and spaces
  - prefix reserved names (`CON`, `PRN`, `AUX`, `NUL`, `COM0–9`, `LPT0–9`, with
    any extension)
- Used by `downloads.rs::safe_file_name` (132) and `commands.rs::clone_repo`
  (930–941). `clone_repo` **rejects** invalid names rather than rewriting them
  and requires exactly one `Normal` path component — blocks `D:evil`, `..`,
  `CON`.
- `downloads.rs`: on Windows `mark_quarantined` writes the Mark-of-the-Web as
  `<file>:Zone.Identifier` with `[ZoneTransfer]\r\nZoneId=3\r\nHostUrl=<url>`
  (never the token) — the counterpart of the xattr quarantine flag. Test under
  `#[cfg(windows)]`.
- `local_index.rs::SKIP_DIRS` (20–37): add `AppData`, `$Recycle.Bin` and
  `System Volume Information`; compare case-insensitively. Don't skip OneDrive
  (Documents is often redirected into it).

### WS7 · Frontend · 1.5–2 PD
- **New `src/lib/platform.ts`** with `isWindows` (from `navigator.userAgent`)
  and a small string table, plus a Vitest test. No `plugin-os` — that would be
  a plugin plus capability for one boolean.

  | macOS | Windows |
  |---|---|
  | Finder | Explorer |
  | Keychain | Windows Credential Manager |
  | ⌘ | Ctrl |
  | menu bar | notification area |
  | System Settings → Notifications | Settings → System → Notifications |
  | log in to macOS | sign in to Windows |

- **Title bar** (`+page.svelte` ~1388, `.tb-spacer` 2636): Windows keeps the
  native title bar; `.titlebar` becomes a normal in-content header there — no
  spacer, no drag region. **No** `decorations:false` (loses Snap Layouts and
  resize borders).
- `+layout.svelte`: set `data-platform`; a global `contextmenu` handler that
  suppresses WebView2's default menu, except on editable fields and selected
  text.
- **Strings:**
  - `+page.svelte`: ~871, 1155, 1467, 1660, 2288, 2319–2362, 2370, 2388, 2424
  - `popover/+page.svelte`: 264, 332, 665, 752
  - `DetailPane.svelte`: 259–261
- Remove the dead `⌘⇧G` label (`popover/+page.svelte:1057`) — no shortcut
  exists on either platform.
- `format.ts::shortenPath` (48–52) and the clone-target preview in
  `DetailPane.svelte` (340/368): split on `/[\\/]/`, rejoin with the input's
  separator; tests in `format.test.ts`.

### WS8 · Release pipeline · 2.5–3 PD
**New `.github/workflows/release-windows.yml`**
- Triggers: `push: tags: ['v*']` and `workflow_dispatch` with a
  `signing-policy` input.
- Runs on `windows-latest`, read-only permissions, environment `release`,
  **no cache** (SignPath reproducibility, cache poisoning).
- Steps:
  1. Check the tag matches the version in `tauri.conf.json`.
  2. `tauri build --no-bundle`.
  3. Upload `gitbuddy.exe`, have SignPath sign it
     (`signpath/github-action-submit-signing-request`), verify with
     `Get-AuthenticodeSignature`.
  4. `tauri bundle --bundles nsis --no-binary-patching` with
     `createUpdaterArtifacts:false`.
  5. Have SignPath sign `setup.exe`; verify.
  6. Upload workflow artifact `windows-installer`: setup.exe + `SHA256SUMS`.
- Every release needs **two manual approvals in SignPath**.
- `bundle.windows.signCommand` is not usable with SignPath Foundation (it runs
  mid-bundle and can't produce the workflow artifact origin verification
  needs). The NSIS uninstaller stays unsigned.

**`scripts/release.sh`**
- Push the tag **early**, right after preflight, so the Windows build runs in
  parallel with the local macOS build and notarisation.
- After the macOS steps:
  1. `gh run watch` and `gh run download`; check SHA256.
  2. Create the **updater signature locally** with
     `tauri signer sign -f ~/.tauri/gitbuddy.key` — the minisign key stays on
     the Mac, in the spirit of DECISIONS 2026-06-10.
  3. `gh release create --draft` with all assets, check the asset list, then
     `gh release edit --draft=false --latest`.
- Drafts don't appear under `/releases/latest/`, so there is no window with an
  incomplete `latest.json` (this also closes today's brief gap).
- Fix the stale-artifact sweep (~290) to keep `*_x64-setup.exe*`.
- New `--windows-run <id>` option to reuse a finished workflow run.

**`scripts/generate-latest-json.sh`**
- Map `release/*_x64-setup.exe` to `windows-x86_64` (the updater looks up
  `windows-x86_64-nsis` first, then `windows-x86_64`; the plain key always
  works).
- Fail hard when any of the three platform keys is missing.
- Emergency flag `--no-windows` omits the key. It must never point at an older
  installer — the version field covers all platforms, so Windows clients would
  reinstall it on every launch.

**Until SignPath has approved:** `signing-policy: none` produces an unsigned
Windows preview, which `release.sh` labels explicitly.

### WS9 · Docs · 1 PD
- **`docs/DECISIONS.md`**, new entries (append-only):
  1. Windows release: NSIS currentUser, SignPath in GitHub Actions, minisign
     local. Amends 2026-06-10 and the PRD non-goal.
  2. Windows tray: the menu trick (2026-09-16) and the native badge
     (2026-11-04) become macOS-only.
  3. Credential Manager and helper path. Amends 2026-09-25.
  4. Mark-of-the-Web, single instance, work-area popover placement.
- **`CLAUDE.md`:** new "Windows-specific quirks" section, commands, CI
  paragraph.
- **README:** Windows install (SmartScreen, pinning the tray icon), code
  signing policy, uninstall notes (Credential Manager entries and the helper
  entries in clones stay behind).
- **`PRD.md`:** new Windows milestone with acceptance criteria.
- **`docs/RELEASING.md`:** SignPath setup, the new flow (early tag, two
  approvals, local minisign, draft → publish), recovery.
- **CHANGELOG.**

### WS10 · End-to-end on the Windows PC plus fix buffer · 2–3 PD
See Verification.

## Effort overview

| Package | PD | Confidence |
|---|---|---|
| WS0 SignPath (own work) | 0.5–1 | high for effort, **low** for calendar time |
| WS1 CI gate + fixes | 1.5 | high |
| WS2 Credential Manager | 0.5 | high |
| WS3 Config/bundling | 0.5–1 | medium |
| WS4 Tray/popover/lifecycle | 3–4 | medium |
| WS5 Editor/terminal/helper | 1.5–2 | medium–high |
| WS6 Filesystem | 1 | high |
| WS7 Frontend | 1.5–2 | medium |
| WS8 Release pipeline | 2.5–3 | medium–low (external service) |
| WS9 Docs | 1 | high |
| WS10 E2E + buffer | 2–3 | medium |
| **Total** | **≈ 16–20** | |

## Risks — spike these first

1. **SignPath:** acceptance, timeline, the "already released" condition,
   privacy and opt-out, whether tags are allowed refs. Apply on day 0 and ask
   these in the application. Fallback: the unsigned preview.
2. **Credential helper as a GUI-subsystem exe** under Git for Windows' MSYS
   `sh` (stdin/stdout over pipes, quoting). Day-1 spike: build a release exe,
   set the helper by hand with a `/` path, then `git credential fill` and
   `git push` from Git Bash, PowerShell and cmd.
3. **Tray behaviour:** click events with the menu attached, the focus race,
   `set_focus` vs. the foreground lock, the icon in the overflow, whether
   `monitor_from_point` takes physical coordinates. Spike: a minimal patched
   build on the PC.
4. **Signing chain:** CLI 2.12 with crate 2.11; is the signature still valid
   after bundling? Simulate in CI with a self-signed cert and `signtool` before
   SignPath exists.
5. **SmartScreen and Smart App Control:** a new hash can still warn despite a
   signature, and the uninstaller stays unsigned. Check on a clean Windows 11.
6. **CRLF / autocrlf:** libgit2 might report clean clones as dirty. Check during
   the clone tests.

## Verification

**Automated**
- `cargo clippy --all-targets -- -D warnings` and `cargo test --lib` green on
  `windows-latest` **and** `macos-latest`.
- `npm run verify` green.
- New unit tests for `popover_origin`, the editor parser, the file-name check,
  `fetch_url` with Windows paths, `helper_command` with a `/` path, and
  `shortenPath` with backslashes.

**Manual on the Windows PC (installed NSIS build)**
1. **Install:** signature "SignPath Foundation", target
   `%LOCALAPPDATA%\gitBuddy`, no UAC prompt, Start-menu entry, WebView2 present
   or downloaded.
2. **First launch:** main window opens; tray icon and tooltip/badge dot show. A
   second launch focuses the running instance — only one tray icon.
3. **Accounts:**
   - OAuth device flow and PAT for GitHub, GitLab and Codeberg.
   - Accounts survive an app restart and a reboot.
   - Entries visible in Credential Manager.
4. **Clone and push:**
   - Clone into a folder with spaces and into `source\repos`.
   - The clone isn't "dirty"; the helper sits in `.git/config` with a `/` path.
   - `git push` works from Git Bash, PowerShell and cmd without a GCM prompt.
   - `D:x`, `CON` and `..` are rejected.
5. **Release download:** `Get-Content <file> -Stream Zone.Identifier` shows
   `ZoneId=3`; SmartScreen warns on launch.
6. **Editor/terminal:** `code` without a console flash, a quoted
   `C:\Program Files\…` path, `wt`, `pwsh`, `cmd` and Git Bash each open in the
   right folder.
7. **Autostart:** after sign-out/sign-in the app starts into the tray, no window.
8. **Notifications:** toast under the name "gitBuddy" with its icon (installed
   builds only — dev builds have no AppUserModelID).
9. **Themes:** System/Light/Dark, the native title bar follows. The tray icon
   follows the taskbar theme; the WebView context menu is suppressed.
10. **Popover:**
    - Taskbar bottom, top and auto-hide; icon from the overflow.
    - Second monitor left and right, including negative coordinates.
    - 100/150/200 % and mixed DPI.
    - Tray click toggles cleanly; clicking outside closes.
    - Right-click menu: "Open gitBuddy" and "Quit" work.
11. **Update N → N+1:**
    - Rehearse first against a staging `latest.json` (endpoint overridden via
      `--config`).
    - Then for real: the passive installer runs, the app relaunches, settings
      and tokens survive, only one instance runs.
12. **Uninstall:** process is stopped, shortcuts gone. With "delete app data"
    ticked, `%APPDATA%\dev.soron2038.gitbuddy` disappears.
13. **macOS regression:**
    - `release.sh --dry-run` passes.
    - The popover sits under the menu bar.
    - The right-click menu works on macOS 27; the badge shows.

## Critical files

| Area | Files |
|---|---|
| Rust core | `src-tauri/src/lib.rs`, `commands.rs`, `keychain.rs`, `git_credential.rs`, `remote_fetch.rs`, `downloads.rs`, `util.rs`, `local_index.rs`, `settings.rs` |
| Config | `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`, new `src-tauri/tauri.windows.conf.json`, `package.json` |
| Frontend | `src/routes/+page.svelte`, `src/routes/popover/+page.svelte`, `src/lib/components/DetailPane.svelte`, `src/lib/format.ts`, new `src/lib/platform.ts` |
| Build and release | `.github/workflows/ci.yml`, new `.github/workflows/release-windows.yml`, `scripts/release.sh`, `scripts/generate-latest-json.sh`, `scripts/regenerate-tray-icon.py` |
| Docs | `docs/DECISIONS.md`, `docs/RELEASING.md`, `CLAUDE.md`, `README.md`, `PRD.md`, new `PRIVACY.md` |
