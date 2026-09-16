# macOS 27: tray left-click opens the context menu instead of the popover

> **Status:** drafted 2026-09-16 — **not executed yet.** Reproduced on one
> MacBook running macOS 27.0 (26A428) with the signed 1.2.0 release; to be
> confirmed on one or two more machines after upgrading them to macOS 27
> before the change is made. Nothing in the app changed between 1.2.0 on
> macOS 26 (works) and 1.2.0 on macOS 27 (broken).

## Context

On macOS 27 a left-click on the gitBuddy tray icon shows the right-click
menu ("Open gitBuddy / Quit gitBuddy") instead of toggling the popover.

**Root cause (confirmed upstream).** `tray-icon` 0.23.1 (pulled in by Tauri
2.11.1, requirement `^0.23`) attaches the menu permanently via
`NSStatusItem.setMenu` and relies on an overlay `NSView` (`TaoTrayTarget`)
placed over the status-bar button to intercept mouse events, so that AppKit
does *not* pop the menu on left-click by itself. macOS 27 no longer forwards
left-clicks to that overlay while an `NSMenu` is attached to the status item
— AppKit shows the menu and our `TrayIconEvent::Click` handler
(`toggle_popover`) never runs.

- Upstream issue: [tauri-apps/tray-icon#355](https://github.com/tauri-apps/tray-icon/issues/355)
  "macOS 27 Beta – click unexpected behavior".
- Fix: [PR #365](https://github.com/tauri-apps/tray-icon/pull/365), merged
  2026-09-16 01:40 UTC, released in **tray-icon 0.25.1** (01:45 UTC). The fix
  attaches the menu only *while it is being presented*:
  `setMenu(Some) → performClick → setMenu(None)`.
- **Not reachable via `cargo update`:** Tauri 2.11.1 requires `tray-icon
  ^0.23`, the newest Tauri 2.11.5 requires `^0.24`; only Tauri 3.0.0-alpha
  requires `^0.25`. A `[patch.crates-io]` pointing at the git repo is ignored
  by cargo because of the semver mismatch.

## Approach: mirror the upstream fix at the app level (no crate patch)

Tauri exposes the raw `tray_icon::TrayIcon` through
`TrayIcon::with_inner_tray_icon`, and `tray_icon::TrayIcon` 0.23.1 already has
`set_menu` and `show_menu` (= `performClick`). That lets us replicate the
upstream trick in ~15 lines of `lib.rs`:

1. Build the tray **without** `.menu(&menu)` → no `NSMenu` on the status item
   → macOS 27 forwards clicks to the overlay again.
2. On `TrayIconEvent::Click { button: Right, button_state: Down }`, attach the
   menu, pop it, detach it.

Rejected alternatives: vendoring `tray-icon` 0.23.1 and back-porting the
patch (~4k lines of third-party code in the repo, must be re-done on every
Tauri bump), and waiting for a Tauri 2.x bump to ≥ 0.25.1 (no timeline; 3.0
is already in alpha). The app-level workaround is also forward-compatible:
once Tauri pulls in a fixed `tray-icon` it stays harmless (upstream's
`show_menu` does the same attach/pop/detach) and `.menu(&menu)` can be
restored at leisure.

### Verified assumptions (Tauri 2.11.1 / tray-icon 0.23.1 sources)

- `run_on_main_thread` executes **inline** when already on the main thread
  (`tauri-runtime-wry::send_user_message`), and tray listeners run inside the
  event-loop callback on the main thread → calling `set_menu` /
  `with_inner_tray_icon` from the handler cannot deadlock. tao's
  `in_callback` guard queues events that arrive during the nested menu
  tracking loop instead of re-entering the callback.
- `MenuEvent`s are delivered to **all** global menu listeners (`app.rs`,
  `EventLoopMessage::MenuEvent`), regardless of whether the menu is currently
  attached to the tray → `on_menu_event` (`open_main` / `quit`) keeps firing;
  the event is processed from the queue after `show_menu()` returns, exactly
  as today.
- `tauri::menu::Menu<R>` is `Clone` (Arc-backed), `Send + Sync`, and
  implements `ContextMenu` → it can be moved into the `on_tray_icon_event`
  closure and passed to `set_menu`.
- `show_menu()` blocks until the menu is dismissed (AppKit tracking loop), so
  `set_menu(None)` immediately afterwards is correct.
- On macOS ≤ 26 the new path behaves identically (menu opens on right
  mouse-down, as today).

## Changes

### 1. `src-tauri/src/lib.rs` — tray setup in the `setup` block (lines ~104–134)

- Drop `.menu(&menu)` from the `TrayIconBuilder`; keep
  `.show_menu_on_left_click(false)`.
- Move `menu` into the `on_tray_icon_event` closure and give the handler two
  arms:
  - `Click { Left, Up, rect, .. }` → `toggle_popover(tray.app_handle(), rect)` (unchanged)
  - `Click { Right, Down, .. }` → `show_tray_menu(tray, &menu)`
- New helper next to `open_main_window` / `toggle_popover`:

```rust
/// macOS 27 stopped forwarding clicks to tray-icon's overlay view while an
/// NSMenu is attached to the NSStatusItem, so a permanently attached menu
/// swallows the left click and pops the menu instead (tauri-apps/tray-icon#355).
/// Mirror upstream's fix (tray-icon 0.25.1, #365) at the app level until a
/// Tauri 2.x release pulls it in: attach the menu only for the duration of
/// the popup. `show_menu` blocks until the menu closes.
fn show_tray_menu(tray: &tauri::tray::TrayIcon, menu: &Menu<tauri::Wry>) {
    let _ = tray.set_menu(Some(menu.clone()));
    let _ = tray.with_inner_tray_icon(|inner| inner.show_menu());
    let _ = tray.set_menu(None::<Menu<tauri::Wry>>);
}
```

- Update the comment above the tray block (`// ── Tray menu (right-click)`)
  to describe the new mechanism. No direct `tray_icon` dependency is needed
  (the type is inferred from the closure bound).

### 2. `docs/DECISIONS.md` — append a new entry (append-only)

`## 2026-09-16 — Tray menu attached only while shown (macOS 27 workaround)`:
symptom, cause (overlay view + `setMenu`), pointers to upstream #355 / #365
and 0.25.1, why no crate patch / vendoring, and the roll-back note: once
Tauri 2.x pulls `tray-icon ≥ 0.25.1`, `.menu(&menu)` may return — but does
not have to.

### 3. `CHANGELOG.md` — under `## [Unreleased]` → `### Fixed`

One line: left-clicking the tray icon on macOS 27 opens the popover again
instead of the context menu.

### 4. `CLAUDE.md` — "Two windows" / macOS quirks

A short paragraph that the tray menu is deliberately not attached on the
builder and why (pointer to the DECISIONS entry), so nobody "cleans it up".

## Verification

The MacBook where the bug reproduces (macOS 27) has Homebrew and the Xcode
CLT but no Rust/Node toolchain and no Developer ID certificate. The bug can
only be reproduced there, so the dev build has to run there.

1. **Install the toolchain on that machine:** `brew install node rustup`,
   then `rustup-init -y` (stable) / `rustup default stable`, make sure
   `~/.cargo/bin` is on `PATH`, then `npm install` in the repo root. The first
   `cargo build` compiles libgit2 + OpenSSL in-tree (several minutes). The
   dev signing wrapper (`src-tauri/scripts/sign-and-run.sh`, ad-hoc
   identifier `dev.soron2038.gitbuddy`) runs automatically via
   `.cargo/config.toml`; Keychain prompts on first launch are expected.
2. `cd src-tauri && cargo check --all-targets && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check && cargo test --lib`
3. `npm run tauri dev` on macOS 27:
   - Left-click on the tray → popover toggles (open/close), no menu.
   - Right-click → "Open gitBuddy / — / Quit gitBuddy" appears on mouse-down;
     "Open gitBuddy" opens the main window; Escape closes the menu and a
     subsequent left-click still works (menu was detached).
   - "Quit gitBuddy" quits the app.
   - Optional: open/close the menu repeatedly — no `RefCell already borrowed`
     panic, no stuck menu state.
4. Sanity check on a macOS ≤ 26 machine: same behaviour as before the change.
5. Afterwards (separately, on the release Mac with the signing key):
   `scripts/build-app.sh` per `docs/RELEASING.md`, release v1.2.1 so the
   updater replaces the installed 1.2.0 on the macOS 27 machines.
