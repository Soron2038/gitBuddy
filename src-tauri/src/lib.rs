// gitBuddy backend entry point.
//
// Responsibilities for the M1 skeleton:
//   * Run as a macOS menu-bar app (no dock icon).
//   * Show a tray icon; left-click toggles the popover window,
//     right-click opens a small menu (Open gitBuddy / Quit).
//   * Hide the popover when it loses focus, the way Mac menu-bar apps behave.
//
// Provider integrations, the local repo index, polling, and notifications
// are scheduled for later milestones (see PRD.md).

use tauri::{
    image::Image,
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, PhysicalPosition, Position,
};

// Only used in release builds — see the focus-hide block in `run()`.
#[cfg(not(debug_assertions))]
use tauri::WindowEvent;

/// Monochrome sprout silhouette embedded at compile time. Designed to read
/// cleanly at 22pt in the macOS menu bar; used as a template image so macOS
/// inverts it for the menu bar appearance (light/dark/translucent).
const TRAY_ICON_PNG: &[u8] = include_bytes!("../icons/tray-icon.png");

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            // gitBuddy lives in the menu bar — no dock icon by default.
            // Opening the main window flips this back to Regular so the
            // window can take focus normally.
            #[cfg(target_os = "macos")]
            {
                app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            }

            // ── Tray menu (right-click) ─────────────────────────────────
            let open_main =
                MenuItem::with_id(app, "open_main", "Open gitBuddy", true, None::<&str>)?;
            let separator = PredefinedMenuItem::separator(app)?;
            let quit = MenuItem::with_id(app, "quit", "Quit gitBuddy", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open_main, &separator, &quit])?;

            // ── Tray icon ───────────────────────────────────────────────
            let tray_icon = Image::from_bytes(TRAY_ICON_PNG)?;
            let _tray = TrayIconBuilder::with_id("main")
                .icon(tray_icon)
                .icon_as_template(true)
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open_main" => open_main_window(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        rect,
                        ..
                    } = event
                    {
                        toggle_popover(tray.app_handle(), rect);
                    }
                })
                .build(app)?;

            // Auto-hide the popover when it loses focus, the way a native
            // NSPopover behaves on macOS. Disabled in debug builds so that
            // screenshots, devtools, and other windows can take focus without
            // the popover disappearing mid-debug.
            #[cfg(not(debug_assertions))]
            if let Some(popover) = app.get_webview_window("popover") {
                let popover_clone = popover.clone();
                popover.on_window_event(move |event| {
                    if let WindowEvent::Focused(false) = event {
                        let _ = popover_clone.hide();
                    }
                });
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
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

fn toggle_popover(app: &tauri::AppHandle, tray_rect: tauri::Rect) {
    let Some(popover) = app.get_webview_window("popover") else {
        return;
    };

    if popover.is_visible().unwrap_or(false) {
        let _ = popover.hide();
        return;
    }

    // Anchor the popover horizontally to the tray icon's center, and place it
    // just below the menu bar. `tray_rect.position` and `.size` are dpi enums
    // (Physical | Logical) — normalise to physical pixels via the window's
    // scale factor.
    let scale = popover.scale_factor().unwrap_or(1.0);
    let tray_pos = tray_rect.position.to_physical::<i32>(scale);
    let tray_size = tray_rect.size.to_physical::<u32>(scale);
    let pop_size = popover.outer_size().unwrap_or_default();

    let x = tray_pos.x + (tray_size.width as i32 / 2) - (pop_size.width as i32 / 2);
    let y = tray_pos.y + tray_size.height as i32 + 4;

    let _ = popover.set_position(Position::Physical(PhysicalPosition { x, y }));
    let _ = popover.show();
    let _ = popover.set_focus();
}
