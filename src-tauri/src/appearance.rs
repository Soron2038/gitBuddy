//! App-wide light/dark appearance, driven by `settings::Appearance`.
//!
//! The setting is applied natively rather than in CSS: forcing a theme sets
//! `NSApp.appearance` on macOS, every WKWebView follows the app's effective
//! appearance, and so the existing `@media (prefers-color-scheme: dark)` rules
//! in `app.css` flip on their own. "System" clears the override and hands the
//! decision back to macOS. The only thing CSS can't reach is the native window
//! background, which this module paints to match.

use crate::settings::Appearance;
use tauri::{window::Color, AppHandle, Manager, Theme, WebviewWindow};

/// Main-window background per scheme. These mirror the `--paper` token in
/// `src/app.css` — the top-level `:root` block (light) and the `:root` inside
/// `@media (prefers-color-scheme: dark)` — and
/// `window_background_matches_app_css_paper` fails when the two drift apart.
pub const PAPER_LIGHT: Color = Color(0xFF, 0xFD, 0xF8, 0xFF);
pub const PAPER_DARK: Color = Color(0x1D, 0x1B, 0x1A, 0xFF);

/// The theme to force for a preference; `None` means "follow macOS".
pub fn forced_theme(pref: Appearance) -> Option<Theme> {
    match pref {
        Appearance::System => None,
        Appearance::Light => Some(Theme::Light),
        Appearance::Dark => Some(Theme::Dark),
    }
}

/// Paint the window background for `theme`.
///
/// `tauri.conf.json` can only carry one literal `backgroundColor`, and it held
/// the light paper tone — so opening the main window in dark mode flashed a
/// white rectangle before the webview drew over it, and the strip around the
/// overlay title bar stayed light. The webview paints its own surface a moment
/// later, but the window beneath is what shows during that moment and around
/// the title bar.
pub fn paint_main_background(window: &WebviewWindow, theme: Theme) {
    let color = match theme {
        Theme::Dark => PAPER_DARK,
        // Light, or a theme this Tauri version doesn't know: the light tone is
        // the safe default, since that's what the app looked like before.
        _ => PAPER_LIGHT,
    };
    if let Err(e) = window.set_background_color(Some(color)) {
        eprintln!("gitbuddy: setting window background failed: {e}");
    }
}

/// Apply an appearance preference to the whole app and repaint the main
/// window's background to match.
///
/// Deliberately goes through `WebviewWindow::set_theme` on every window rather
/// than `AppHandle::set_theme`. On macOS both end in the same app-wide
/// `NSApp.appearance`, but tao's window-level call also refreshes that
/// window's cached theme; the app-level one leaves every window's cache stale,
/// so `window.theme()` — which the "System" branch below relies on — would
/// report the previous appearance. On Linux/Windows the theme is per window
/// anyway, so the loop is required there.
pub fn apply(app: &AppHandle, pref: Appearance) {
    let forced = forced_theme(pref);
    for (label, window) in app.webview_windows() {
        if let Err(e) = window.set_theme(forced) {
            eprintln!("gitbuddy: setting theme on window {label:?} failed: {e}");
        }
    }
    if let Some(main) = app.get_webview_window("main") {
        // With the override cleared, the refreshed cache holds the system
        // appearance. Commands off the main thread are fine too: the getter
        // queues behind the `set_theme` messages sent above.
        let theme = forced.or_else(|| main.theme().ok()).unwrap_or(Theme::Light);
        paint_main_background(&main, theme);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forced_theme_maps_every_preference() {
        assert_eq!(forced_theme(Appearance::System), None);
        assert_eq!(forced_theme(Appearance::Light), Some(Theme::Light));
        assert_eq!(forced_theme(Appearance::Dark), Some(Theme::Dark));
    }

    /// Drop `/* … */` comments and all whitespace, so the lookups below don't
    /// trip over prose in comments or formatting choices.
    fn normalise_css(css: &str) -> String {
        let mut out = String::with_capacity(css.len());
        let mut rest = css;
        while let Some(start) = rest.find("/*") {
            out.push_str(&rest[..start]);
            rest = rest[start + 2..]
                .split_once("*/")
                .map_or("", |(_, after)| after);
        }
        out.push_str(rest);
        out.retain(|c| !c.is_whitespace());
        out
    }

    /// The `--paper` value of the first `:root{` block at or after `from`,
    /// parsed from `#RRGGBB`.
    fn paper_after(css: &str, from: usize) -> (u8, u8, u8) {
        let block = &css[from..];
        let block = &block[block.find(":root{").expect("a :root block")..];
        let block = &block[..block.find('}').expect("the :root block to close")];
        let hex = block
            .split(['{', ';'])
            .find_map(|decl| decl.strip_prefix("--paper:"))
            .expect("a --paper declaration in the :root block");
        let hex = hex.strip_prefix('#').expect("--paper to be a hex color");
        assert_eq!(hex.len(), 6, "--paper should be #RRGGBB, got #{hex}");
        let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex digits");
        (channel(0), channel(2), channel(4))
    }

    fn rgb(c: Color) -> (u8, u8, u8) {
        (c.0, c.1, c.2)
    }

    #[test]
    fn window_background_matches_app_css_paper() {
        let css = normalise_css(include_str!("../../src/app.css"));

        let light = paper_after(&css, 0);
        let dark_media = css
            .find("(prefers-color-scheme:dark)")
            .expect("a prefers-color-scheme: dark block in app.css");
        let dark = paper_after(&css, dark_media);

        assert_eq!(
            rgb(PAPER_LIGHT),
            light,
            "PAPER_LIGHT is out of sync with the light --paper in src/app.css"
        );
        assert_eq!(
            rgb(PAPER_DARK),
            dark,
            "PAPER_DARK is out of sync with the dark --paper in src/app.css"
        );
    }
}
