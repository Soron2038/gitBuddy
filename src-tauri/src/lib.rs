// gitBuddy backend entry point.
//
// M1: menu-bar tray, popover, main window.
// M2: GitHub provider (PAT auth + waiting-items search) + Keychain storage.
// Local repo index, polling, notifications, multi-account, GitLab/Codeberg
// come in later milestones — see PRD.md.

mod accounts;
mod aggregator;
mod appearance;
mod codeberg;
mod commands;
mod downloads;
mod git_credential;
mod github;
mod gitlab;
mod keychain;
mod launcher;
mod local_index;
mod notifications;
mod oauth;
mod popover;
mod provider_util;
mod remote_fetch;
mod settings;
#[cfg(target_os = "macos")]
mod tray_badge;
#[cfg(windows)]
mod tray_windows;
mod types;
mod util;

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use commands::AppState;
#[cfg(not(windows))]
use tauri::image::Image;
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, PhysicalPosition, Position, WindowEvent,
};

/// Monochrome sprout silhouette embedded at compile time. Designed to read
/// cleanly at 22pt in the macOS menu bar; used as a template image so macOS
/// inverts it for the menu bar appearance (light/dark/translucent).
///
/// Regenerate with `python3 scripts/regenerate-tray-icon.py` — the SVG next
/// to this PNG is the design reference, the Python script the canonical
/// renderer (system-level SVG-to-PNG converters on macOS are unreliable for
/// this kind of small template image). Windows doesn't tint tray icons and
/// uses its own set instead (`tray_windows.rs`).
#[cfg(not(windows))]
const TRAY_ICON_PNG: &[u8] = include_bytes!("../icons/tray-icon.png");

/// When the popover last hid itself because it lost focus — see
/// `popover::REOPEN_GUARD`. Only release builds auto-hide, so in a debug
/// build this stays `None`.
static POPOVER_AUTO_HIDDEN_AT: Mutex<Option<Instant>> = Mutex::new(None);

/// Logical pixels between the popover and the taskbar, the tray icon and the
/// edges of the work area on Windows: the margin Windows 11's own flyouts keep.
#[cfg(not(target_os = "macos"))]
const POPOVER_GAP: f64 = 12.0;

/// The argument the Windows autostart entry launches gitBuddy with, so a
/// launch at sign-in can stay in the tray while any other launch opens the
/// main window (Windows 11 tucks a new tray icon into the overflow, so the app
/// would otherwise seem not to start at all).
#[cfg(windows)]
const AUTOSTART_ARG: &str = "--autostart";

#[cfg(windows)]
fn launched_by_autostart(args: &[String]) -> bool {
    args.iter().any(|arg| arg == AUTOSTART_ARG)
}

/// Command-line modes that run *instead of* the app. Today that is only the
/// git credential helper (`gitbuddy credential …`, see `git_credential.rs`),
/// which git runs from any terminal while the app itself may or may not be
/// open. Returns the exit code when `args` selected such a mode.
pub fn run_cli(args: &[String]) -> Option<i32> {
    git_credential::run_from_args(args)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default();
    // A second launch hands its arguments to the running app and exits, so
    // there is never a second tray icon. It has to be the first plugin. The
    // credential helper is unaffected: `run_cli` handles it before this.
    #[cfg(windows)]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
        if !launched_by_autostart(&args) {
            open_main_window(app);
        }
    }));
    builder
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_notification::init())
        // "Start at login" (PRD §4.9).
        .plugin(autostart_plugin())
        // `relaunch` after the updater installs a new bundle (PRD §6.5).
        .plugin(tauri_plugin_process::init())
        .manage(Arc::new(AppState::default()))
        .invoke_handler(tauri::generate_handler![
            commands::provider_set_token,
            commands::gh_oauth_begin,
            commands::gh_oauth_poll,
            commands::accounts_list,
            commands::accounts_disconnect,
            commands::open_main,
            commands::open_main_settings,
            commands::open_main_add_account,
            commands::list_waiting,
            commands::list_repos,
            commands::list_releases,
            commands::list_ci,
            commands::list_local_repos,
            commands::aggregator_refresh_now,
            commands::last_sync_info,
            commands::clone_repo,
            commands::download_release_asset,
            commands::enable_git_push,
            commands::rescan_local_repos,
            commands::get_settings,
            commands::save_settings,
            commands::export_config,
            commands::import_config,
            commands::run_editor,
            commands::run_terminal,
        ])
        .setup(|app| {
            // In-app auto-update (PRD §6.5). Desktop-only; the frontend drives
            // the actual `check()` (silent on launch + a Settings button).
            #[cfg(desktop)]
            app.handle()
                .plugin(tauri_plugin_updater::Builder::new().build())?;

            // gitBuddy lives in the menu bar — no dock icon by default.
            // Opening the main window flips this back to Regular so the
            // window can take focus normally.
            #[cfg(target_os = "macos")]
            {
                app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            }

            // Before anything can open a libgit2 connection.
            remote_fetch::init_libgit2_timeouts();

            // ── Tray menu (right-click) ─────────────────────────────────
            // Deliberately NOT passed to `TrayIconBuilder::menu`. A menu that
            // is permanently attached to the NSStatusItem swallows the left
            // click on macOS 27 (AppKit pops the menu before tray-icon's
            // overlay view sees the event). Instead the menu is attached only
            // while it is being shown — see `show_tray_menu` and the
            // 2026-09-16 entry in docs/DECISIONS.md.
            let open_main =
                MenuItem::with_id(app, "open_main", "Open gitBuddy", true, None::<&str>)?;
            let separator = PredefinedMenuItem::separator(app)?;
            let quit = MenuItem::with_id(app, "quit", "Quit gitBuddy", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open_main, &separator, &quit])?;

            // ── Tray icon ───────────────────────────────────────────────
            #[cfg(not(windows))]
            let tray_icon = Image::from_bytes(TRAY_ICON_PNG)?;
            #[cfg(windows)]
            let tray_icon = tray_windows::initial_icon()?;
            let tray = TrayIconBuilder::with_id("main")
                .icon(tray_icon)
                .icon_as_template(true)
                .show_menu_on_left_click(false);
            // Outside macOS the menu is attached for good: tray-icon pops it
            // on right mouse-up and still reports the left click. The
            // attach-while-shown trick would lose the menu's clicks on
            // Windows, where tray-icon's `show_menu` doesn't wait for the
            // choice and the menu's window subclass is gone by the time the
            // WM_COMMAND arrives.
            #[cfg(not(target_os = "macos"))]
            let tray = tray.menu(&menu);
            let _tray = tray
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open_main" => open_main_window(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(move |tray, event| match event {
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        rect,
                        ..
                    } => toggle_popover(tray.app_handle(), rect),
                    #[cfg(target_os = "macos")]
                    TrayIconEvent::Click {
                        button: MouseButton::Right,
                        button_state: MouseButtonState::Down,
                        ..
                    } => show_tray_menu(tray, &menu),
                    _ => {}
                })
                .build(app)?;

            // Spawn the aggregator polling task — after the tray exists, since
            // every tick updates its badge. The first thing it does is
            // call `ensure_initialized` (keychain restore + account
            // migrations), so all subsequent ticks see fully-hydrated
            // provider HashMaps. Both windows subscribe to the `data-updated`
            // event the loop emits and re-read their views via the
            // `list_*` commands, which are cache reads.
            let state = app.state::<Arc<AppState>>().inner().clone();
            aggregator::spawn_loop(app.handle().clone(), state);

            // Auto-hide the popover when it loses focus, the way a native
            // NSPopover behaves on macOS. Disabled in debug builds so that
            // screenshots, devtools, and other windows can take focus without
            // the popover disappearing mid-debug.
            #[cfg(not(debug_assertions))]
            if let Some(popover) = app.get_webview_window("popover") {
                let popover_clone = popover.clone();
                popover.on_window_event(move |event| {
                    if let WindowEvent::Focused(false) = event {
                        // Only a hide that actually closes something counts;
                        // `hide()` itself is followed by a blur, and that one
                        // mustn't swallow the next tray click.
                        if popover_clone.is_visible().unwrap_or(false) {
                            *POPOVER_AUTO_HIDDEN_AT
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner) = Some(Instant::now());
                        }
                        let _ = popover_clone.hide();
                    }
                });
            }

            // Apply the saved appearance (System / Light / Dark) before either
            // window is first shown. It's app-wide, so it runs whether or not
            // `main` exists, and it also paints the main window's background
            // for the resulting theme — see `appearance::paint_main_background`
            // for why the window beneath the webview needs that. An unreadable
            // settings file falls back to following macOS.
            appearance::apply(
                app.handle(),
                settings::load(app.handle())
                    .map(|s| s.appearance)
                    .unwrap_or_default(),
            );

            // Close-to-hide on the main window. macOS convention for menu-bar
            // apps: Cmd+W (or red traffic light) shouldn't quit the app, it
            // should hide the window — the popover and tray stay live. We
            // also flip the activation policy back to Accessory so the dock
            // icon disappears, signalling "main window is closed".
            if let Some(main) = app.get_webview_window("main") {
                let main_clone = main.clone();
                // Only the activation-policy flip below needs it; elsewhere
                // it would be an unused variable (clippy -D warnings on the
                // Windows CI job).
                #[cfg(target_os = "macos")]
                let app_handle = app.handle().clone();
                main.on_window_event(move |event| match event {
                    WindowEvent::CloseRequested { api, .. } => {
                        api.prevent_close();
                        let _ = main_clone.hide();
                        #[cfg(target_os = "macos")]
                        {
                            let _ = app_handle
                                .set_activation_policy(tauri::ActivationPolicy::Accessory);
                        }
                    }
                    // A live macOS appearance switch while "System" is
                    // selected: repaint so the strip around the title bar
                    // doesn't stay in the old scheme. tao only emits this for
                    // the system notification, and compares against
                    // NSApp.effectiveAppearance — which already reflects a
                    // forced Light/Dark — so while an override is active
                    // nothing fires and the payload is always the theme to
                    // paint.
                    WindowEvent::ThemeChanged(theme) => {
                        appearance::paint_main_background(&main_clone, *theme);
                        // A theme switch usually flips the taskbar too.
                        #[cfg(windows)]
                        tray_windows::refresh(main_clone.app_handle());
                    }
                    _ => {}
                });
            }

            // Windows tucks a new tray icon into the overflow, so a launch
            // from the Start menu or the installer would show nothing at all.
            // Only the launch at sign-in stays in the tray.
            #[cfg(windows)]
            if !launched_by_autostart(&std::env::args().collect::<Vec<_>>()) {
                open_main_window(app.handle());
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// Show how many local clones are behind their upstream as a badge on the
/// tray icon, and say so in the tooltip. Nothing behind clears both.
pub(crate) fn set_tray_badge(app: &tauri::AppHandle, behind: usize) {
    let Some(tray) = app.tray_by_id("main") else {
        return;
    };
    let tooltip = match behind {
        0 => "gitBuddy".to_string(),
        1 => "gitBuddy — 1 local clone is behind its remote".to_string(),
        n => format!("gitBuddy — {n} local clones are behind their remotes"),
    };
    #[cfg(target_os = "macos")]
    tray_badge::show(&tray, behind);
    #[cfg(windows)]
    tray_windows::show(&tray, behind);
    let _ = tray.set_tooltip(Some(tooltip));
}

/// The autostart plugin. On macOS a LaunchAgent without arguments: gitBuddy
/// starts into the menu bar either way. On Windows a Run key that passes
/// [`AUTOSTART_ARG`], so only that launch skips the main window.
fn autostart_plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    let builder = tauri_plugin_autostart::Builder::new();
    #[cfg(windows)]
    let builder = builder.arg(AUTOSTART_ARG);
    builder.build()
}

fn open_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        // Bring the dock icon back so the window can be a normal app window.
        // `AppHandle::set_activation_policy` returns `Result`; ignore failures —
        // worst case the dock icon stays hidden and the user can still see the
        // window in the alt-tab switcher.
        #[cfg(target_os = "macos")]
        {
            let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
        }
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// Show the tray context menu, attaching it to the status item only for the
/// duration of the popup.
///
/// macOS 27 stopped forwarding clicks to tray-icon's overlay view while an
/// `NSMenu` is attached to the `NSStatusItem`, so a permanently attached menu
/// swallows the left click and pops the menu instead
/// (tauri-apps/tray-icon#355). This mirrors upstream's fix (tray-icon 0.25.1,
/// PR #365: `setMenu(Some) → performClick → setMenu(None)`) at the app level
/// until a Tauri 2.x release pulls that version in. `show_menu` blocks until
/// the menu is dismissed, so detaching right after it returns is correct.
///
/// Runs inside the tray event handler, i.e. on the main thread; Tauri's
/// `run_on_main_thread` executes inline there, so this cannot deadlock.
///
/// macOS only: other platforms attach the menu for good (see the tray setup).
#[cfg(target_os = "macos")]
fn show_tray_menu(tray: &tauri::tray::TrayIcon, menu: &Menu<tauri::Wry>) {
    // A failed attach only degrades to "no menu this time"; never worth a panic.
    let _ = tray.set_menu(Some(menu.clone()));
    let _ = tray.with_inner_tray_icon(|inner| inner.show_menu());
    let _ = tray.set_menu(None::<Menu<tauri::Wry>>);
}

fn toggle_popover(app: &tauri::AppHandle, tray_rect: tauri::Rect) {
    let Some(popover) = app.get_webview_window("popover") else {
        return;
    };

    if popover.is_visible().unwrap_or(false) {
        let _ = popover.hide();
        return;
    }

    let auto_hidden_at = POPOVER_AUTO_HIDDEN_AT
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take();
    if popover::closed_by_this_click(auto_hidden_at, Instant::now()) {
        return;
    }

    #[cfg(windows)]
    tray_windows::refresh(app);

    #[cfg(not(target_os = "macos"))]
    let scale = popover.scale_factor().unwrap_or(1.0);
    let _ = popover.set_position(popover_position(app, &popover, tray_rect));
    let _ = popover.show();
    // Opening on a monitor with another scale factor resizes the window
    // (per-monitor DPI), which can also move it: place it again at its size
    // there.
    #[cfg(not(target_os = "macos"))]
    if popover.scale_factor().unwrap_or(scale) != scale {
        let _ = popover.set_position(popover_position(app, &popover, tray_rect));
    }
    let _ = popover.set_focus();
}

/// macOS: centred on the menu-bar icon, just below the menu bar.
#[cfg(target_os = "macos")]
fn popover_position(
    _app: &tauri::AppHandle,
    popover: &tauri::WebviewWindow,
    tray_rect: tauri::Rect,
) -> Position {
    // `tray_rect.position` and `.size` are dpi enums (Physical | Logical) —
    // normalise to physical pixels via the window's scale factor.
    let scale = popover.scale_factor().unwrap_or(1.0);
    let tray_pos = tray_rect.position.to_physical::<i32>(scale);
    let tray_size = tray_rect.size.to_physical::<u32>(scale);
    let pop_size = popover.outer_size().unwrap_or_default();

    let x = tray_pos.x + (tray_size.width as i32 / 2) - (pop_size.width as i32 / 2);
    let y = tray_pos.y + tray_size.height as i32 + 4;
    Position::Physical(PhysicalPosition { x, y })
}

/// Elsewhere: away from the taskbar, inside the work area of the monitor the
/// tray icon is on (see `popover::popover_origin`).
#[cfg(not(target_os = "macos"))]
fn popover_position(
    app: &tauri::AppHandle,
    popover: &tauri::WebviewWindow,
    tray_rect: tauri::Rect,
) -> Position {
    use popover::PxRect;

    // tray-icon reports the rect in physical pixels on Windows, which makes
    // this conversion a no-op there.
    let scale = popover.scale_factor().unwrap_or(1.0);
    let pos = tray_rect.position.to_physical::<i32>(scale);
    let size = tray_rect.size.to_physical::<u32>(scale);
    let tray = PxRect::new(pos.x, pos.y, size.width, size.height);
    let (cx, cy) = tray.center();

    let contains = |m: &tauri::Monitor| {
        let (p, s) = (m.position(), m.size());
        PxRect::new(p.x, p.y, s.width, s.height).contains(cx, cy)
    };
    let monitor = app
        .monitor_from_point(f64::from(cx), f64::from(cy))
        .ok()
        .flatten()
        .or_else(|| {
            app.available_monitors()
                .ok()
                .and_then(|all| all.into_iter().find(|m| contains(m)))
        })
        .or_else(|| app.primary_monitor().ok().flatten());
    let Some(monitor) = monitor else {
        // No monitor information at all: still open, at the icon.
        return Position::Physical(PhysicalPosition {
            x: tray.x,
            y: tray.y,
        });
    };

    // The popover's size on that monitor: its logical size at that monitor's
    // scale factor, which differs from the current one with mixed DPI.
    let target_scale = monitor.scale_factor();
    let pop = popover
        .outer_size()
        .unwrap_or_default()
        .to_logical::<f64>(scale)
        .to_physical::<u32>(target_scale);
    let (mp, ms) = (monitor.position(), monitor.size());
    let work = monitor.work_area();
    let (x, y) = popover::popover_origin(
        tray,
        (pop.width, pop.height),
        PxRect::new(mp.x, mp.y, ms.width, ms.height),
        PxRect::new(
            work.position.x,
            work.position.y,
            work.size.width,
            work.size.height,
        ),
        (POPOVER_GAP * target_scale).round() as i32,
    );
    Position::Physical(PhysicalPosition { x, y })
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn only_the_autostart_launch_stays_in_the_tray() {
        let args = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(launched_by_autostart(&args(&[
            r"C:\Users\me\AppData\Local\gitBuddy\gitbuddy.exe",
            "--autostart",
        ])));
        assert!(!launched_by_autostart(&args(&[
            r"C:\Users\me\AppData\Local\gitBuddy\gitbuddy.exe"
        ])));
        // The credential helper never gets here (`run_cli`), but its
        // arguments must not read as an autostart either.
        assert!(!launched_by_autostart(&args(&[
            "gitbuddy.exe",
            "credential",
            "get"
        ])));
    }
}
