//! The tray icon on Windows: one per taskbar theme, with a terracotta dot
//! while local clones are behind their upstream.
//!
//! macOS tints a template image to match the menu bar and draws the count in
//! a native view (`tray_badge.rs`). Windows does neither: a notification-area
//! icon is shown exactly as given, and digits don't read at 16 px. So there
//! are four ready-made PNGs (`scripts/regenerate-tray-icon.py`), picked by the
//! taskbar theme and whether anything is behind. The count itself is in the
//! tooltip, which `set_tray_badge` in `lib.rs` sets on every platform.
//!
//! The taskbar follows the *Windows* mode (`SystemUsesLightTheme`), not the
//! app mode (`AppsUseLightTheme`) that the webviews and tao's `ThemeChanged`
//! follow; the two can differ. Nothing notifies us when only the Windows mode
//! changes, so the theme is re-read whenever the icon might change: on every
//! scan (which sets the badge), on each tray click and on `ThemeChanged`.

use std::sync::{Mutex, PoisonError};

use tauri::{image::Image, tray::TrayIcon, AppHandle};
use winreg::{enums::HKEY_CURRENT_USER, RegKey};

const LIGHT: &[u8] = include_bytes!("../icons/tray-win-light.png");
const LIGHT_BADGED: &[u8] = include_bytes!("../icons/tray-win-light-badged.png");
const DARK: &[u8] = include_bytes!("../icons/tray-win-dark.png");
const DARK_BADGED: &[u8] = include_bytes!("../icons/tray-win-dark-badged.png");

const PERSONALIZE: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";

/// Which of the four icons the tray shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Variant {
    light_taskbar: bool,
    badged: bool,
}

impl Variant {
    fn png(self) -> &'static [u8] {
        match (self.light_taskbar, self.badged) {
            (true, false) => LIGHT,
            (true, true) => LIGHT_BADGED,
            (false, false) => DARK,
            (false, true) => DARK_BADGED,
        }
    }
}

/// What the tray shows now, so `set_icon` only runs when that changes. Every
/// scan calls `show`, and swapping the icon makes the shell redraw it.
static SHOWN: Mutex<Option<Variant>> = Mutex::new(None);

/// Whether the taskbar is light. Dark is the Windows 11 default, and also the
/// answer when the value can't be read (older builds, a locked-down profile).
fn taskbar_is_light() -> bool {
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(PERSONALIZE)
        .and_then(|key| key.get_value::<u32, _>("SystemUsesLightTheme"))
        .is_ok_and(|value| value != 0)
}

/// The icon the tray starts with: nothing is behind yet.
pub(crate) fn initial_icon() -> tauri::Result<Image<'static>> {
    let variant = Variant {
        light_taskbar: taskbar_is_light(),
        badged: false,
    };
    *SHOWN.lock().unwrap_or_else(PoisonError::into_inner) = Some(variant);
    Image::from_bytes(variant.png())
}

/// Show the dot when `behind > 0`, in the colours of the current taskbar.
/// Called from the aggregator's scans, off the main thread.
pub(crate) fn show(tray: &TrayIcon, behind: usize) {
    let tray = tray.clone();
    let _ = tray
        .app_handle()
        .clone()
        .run_on_main_thread(move || update(&tray, Some(behind > 0)));
}

/// Re-read the taskbar theme and keep the badge as it is.
pub(crate) fn refresh(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(tray) = handle.tray_by_id("main") {
            update(&tray, None);
        }
    });
}

/// Every swap runs here, on the main thread, where Tauri runs `set_icon`
/// inline. Off it, `set_icon` blocks until the main thread has run it, so
/// holding `SHOWN` across it from another thread could deadlock against a
/// tray click; on the main thread the swaps also can't interleave.
fn update(tray: &TrayIcon, badged: Option<bool>) {
    let mut shown = SHOWN.lock().unwrap_or_else(PoisonError::into_inner);
    let variant = Variant {
        light_taskbar: taskbar_is_light(),
        badged: badged.unwrap_or_else(|| shown.is_some_and(|v| v.badged)),
    };
    if *shown == Some(variant) {
        return;
    }
    // A failed swap keeps the old icon and is retried on the next call.
    let Ok(icon) = Image::from_bytes(variant.png()) else {
        return;
    };
    if tray.set_icon(Some(icon)).is_ok() {
        *shown = Some(variant);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Variant; 4] = [
        Variant {
            light_taskbar: true,
            badged: false,
        },
        Variant {
            light_taskbar: true,
            badged: true,
        },
        Variant {
            light_taskbar: false,
            badged: false,
        },
        Variant {
            light_taskbar: false,
            badged: true,
        },
    ];

    #[test]
    fn every_variant_is_its_own_square_32_px_icon() {
        for variant in ALL {
            let icon = Image::from_bytes(variant.png()).expect("embedded PNG decodes");
            assert_eq!((icon.width(), icon.height()), (32, 32), "{variant:?}");
        }
        for (i, a) in ALL.iter().enumerate() {
            for b in &ALL[i + 1..] {
                assert_ne!(a.png(), b.png(), "{a:?} and {b:?} share a PNG");
            }
        }
    }

    #[test]
    fn the_light_taskbar_gets_the_dark_glyph() {
        // The fully opaque pixels are the glyph's. A light taskbar needs a
        // dark glyph and vice versa, or the robot disappears into it.
        fn glyph_luma(png: &[u8]) -> u32 {
            let icon = Image::from_bytes(png).unwrap();
            let opaque: Vec<&[u8; 4]> = icon
                .rgba()
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|px| px[3] == 255)
                .collect();
            assert!(!opaque.is_empty());
            let sum: u32 = opaque
                .iter()
                .map(|px| (u32::from(px[0]) + u32::from(px[1]) + u32::from(px[2])) / 3)
                .sum();
            sum / opaque.len() as u32
        }
        assert!(glyph_luma(LIGHT) < 64, "glyph on a light taskbar is dark");
        assert!(glyph_luma(DARK) > 192, "glyph on a dark taskbar is light");
    }
}
