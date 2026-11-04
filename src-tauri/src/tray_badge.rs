//! The count badge on the tray icon: how many local clones are behind.
//!
//! The badge is terracotta, and a template image can't carry colour. Making the
//! whole icon a plain image instead would mean tinting the robot ourselves, and
//! the menu bar's tint follows the wallpaper as well as the system appearance.
//! So the robot stays a template that macOS keeps tinting, and the badge is a
//! small native view on the status item's button, drawn into a round gap in
//! `tray-icon-badged.png`. See the 2026-11-04 entry in `docs/DECISIONS.md`.

use std::cell::RefCell;
use std::sync::atomic::{AtomicUsize, Ordering};

use objc2::rc::Retained;
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAppearanceCustomization, NSAppearanceNameAqua, NSAppearanceNameDarkAqua,
    NSAttributedStringNSStringDrawing, NSBezierPath, NSColor, NSFont, NSFontAttributeName,
    NSForegroundColorAttributeName, NSStatusItem, NSView,
};
use objc2_foundation::{
    NSArray, NSAttributedString, NSAttributedStringKey, NSDictionary, NSPoint, NSRect, NSSize,
    NSString,
};
use tauri::image::Image;
use tauri::tray::TrayIcon;

/// Same robot, with the gap for the badge. Regenerate with
/// `scripts/regenerate-tray-icon.py`, which holds the same geometry.
const BADGED_ICON_PNG: &[u8] = include_bytes!("../icons/tray-icon-badged.png");
const PLAIN_ICON_PNG: &[u8] = include_bytes!("../icons/tray-icon.png");

/// Badge geometry in the icon's design units, y pointing down. tray-icon draws
/// the 22-unit icon 18 pt tall, so 1 unit is about 0.82 pt: this makes the badge
/// 10 pt across, the smallest that stays readable on a non-Retina display. It
/// hangs below the icon's canvas, into the button's margin.
const GRID_WIDTH: f64 = 27.0;
const BADGE_CX: f64 = 20.8;
const BADGE_CY: f64 = 18.3;
const BADGE_R: f64 = 6.1;

const DIGIT_SIZE: f64 = 9.8;
const TWO_CHAR_SIZE: f64 = 7.4;

/// `--terracotta` from `src/app.css`, per scheme, with a digit colour that reads
/// on it. `badge_matches_app_css_terracotta` fails when they drift apart.
const LIGHT_FILL: u32 = 0xB55436;
const DARK_FILL: u32 = 0xED8F6A;
const LIGHT_DIGIT: u32 = 0xFFFFFF;
const DARK_DIGIT: u32 = 0x2A1710;

/// What the badge says, or `None` for no badge. One circle has room for two
/// characters at most.
fn label(behind: usize) -> Option<String> {
    match behind {
        0 => None,
        1..=9 => Some(behind.to_string()),
        _ => Some("9+".to_string()),
    }
}

/// Where the badge goes inside the button, given where the button drew the
/// icon. `flipped` is the button's coordinate system.
fn badge_frame(image: NSRect, flipped: bool) -> NSRect {
    let unit = image.size.width / GRID_WIDTH;
    let side = 2.0 * BADGE_R * unit;
    let x = image.origin.x + (BADGE_CX - BADGE_R) * unit;
    let y = if flipped {
        image.origin.y + (BADGE_CY - BADGE_R) * unit
    } else {
        image.origin.y + image.size.height - (BADGE_CY + BADGE_R) * unit
    };
    NSRect::new(NSPoint::new(x, y), NSSize::new(side, side))
}

fn color(rgb: u32) -> Retained<NSColor> {
    let channel = |shift: u32| f64::from((rgb >> shift) & 0xFF) / 255.0;
    NSColor::colorWithSRGBRed_green_blue_alpha(channel(16), channel(8), channel(0), 1.0)
}

struct Ivars {
    label: RefCell<String>,
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    struct BadgeView;

    impl BadgeView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            self.draw();
        }

        // Clicks belong to tray-icon's own overlay view.
        #[unsafe(method(hitTest:))]
        fn hit_test(&self, _point: NSPoint) -> *mut NSView {
            std::ptr::null_mut()
        }

        // The menu bar's appearance can change without any tick of ours.
        #[unsafe(method(viewDidChangeEffectiveAppearance))]
        fn appearance_changed(&self) {
            self.setNeedsDisplay(true);
        }
    }
);

impl BadgeView {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            label: RefCell::new(String::new()),
        });
        unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] }
    }

    fn is_dark(&self) -> bool {
        let names =
            NSArray::from_slice(unsafe { &[NSAppearanceNameAqua, NSAppearanceNameDarkAqua] });
        self.effectiveAppearance()
            .bestMatchFromAppearancesWithNames(&names)
            .is_some_and(|name| &*name == unsafe { NSAppearanceNameDarkAqua })
    }

    fn draw(&self) {
        let (fill, digit) = if self.is_dark() {
            (DARK_FILL, DARK_DIGIT)
        } else {
            (LIGHT_FILL, LIGHT_DIGIT)
        };
        let bounds = self.bounds();
        color(fill).setFill();
        NSBezierPath::bezierPathWithOvalInRect(bounds).fill();

        let label = self.ivars().label.borrow();
        let unit = bounds.size.width / (2.0 * BADGE_R);
        let size = if label.chars().count() > 1 {
            TWO_CHAR_SIZE
        } else {
            DIGIT_SIZE
        };
        let font = NSFont::boldSystemFontOfSize(size * unit);
        let digit_color = color(digit);
        let keys: [&NSAttributedStringKey; 2] =
            unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
        let values: [&objc2::runtime::AnyObject; 2] = [&font, &digit_color];
        let attributes = NSDictionary::from_slices(&keys, &values);
        let text = unsafe {
            NSAttributedString::new_with_attributes(&NSString::from_str(&label), &attributes)
        };

        // Centre on the digits' cap height: the line box also holds the
        // descender, which would push them above the middle.
        let baseline = (bounds.size.height - font.capHeight()) / 2.0;
        text.drawAtPoint(NSPoint::new(
            (bounds.size.width - text.size().width) / 2.0,
            baseline + font.descender(),
        ));
    }
}

thread_local! {
    // Main thread only, like the view itself.
    static VIEW: RefCell<Option<Retained<BadgeView>>> = const { RefCell::new(None) };
}

/// What the tray shows at the moment. `store_locals` lands here on every tick;
/// most ticks change nothing, and swapping the icon anyway would flicker.
static SHOWN: AtomicUsize = AtomicUsize::new(0);

/// Show `behind` as the badge, or remove it at zero. Any thread.
pub fn show(tray: &TrayIcon, behind: usize) {
    let previous = SHOWN.swap(behind, Ordering::Relaxed);
    if previous == behind {
        return;
    }
    if (previous == 0) != (behind == 0) {
        let png = if behind == 0 {
            PLAIN_ICON_PNG
        } else {
            BADGED_ICON_PNG
        };
        let icon = Image::from_bytes(png).expect("tray icon PNGs are embedded at build time");
        if let Err(e) = tray.set_icon_with_as_template(Some(icon), true) {
            eprintln!("gitbuddy: swapping the tray icon failed: {e}");
        }
    }
    let text = label(behind);
    let result = tray.with_inner_tray_icon(move |inner| {
        if let (Some(item), Some(mtm)) = (inner.ns_status_item(), MainThreadMarker::new()) {
            place(&item, text.as_deref(), mtm);
        }
    });
    if let Err(e) = result {
        eprintln!("gitbuddy: updating the tray badge failed: {e}");
    }
}

fn place(item: &NSStatusItem, text: Option<&str>, mtm: MainThreadMarker) {
    let Some(button) = item.button(mtm) else {
        return;
    };
    VIEW.with_borrow_mut(|slot| {
        let view = slot.get_or_insert_with(|| {
            let view = BadgeView::new(mtm);
            button.addSubview(&view);
            view
        });
        view.setHidden(text.is_none());
        let Some(text) = text else {
            return;
        };
        *view.ivars().label.borrow_mut() = text.to_string();

        // The button resizes for the wider icon during layout.
        button.layoutSubtreeIfNeeded();
        let Some(cell) = button.cell() else {
            return;
        };
        let image = cell.imageRectForBounds(button.bounds());
        view.setFrame(badge_frame(image, button.isFlipped()));
        view.setNeedsDisplay(true);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_caps_at_two_characters() {
        assert_eq!(label(0), None);
        assert_eq!(label(1).as_deref(), Some("1"));
        assert_eq!(label(9).as_deref(), Some("9"));
        assert_eq!(label(10).as_deref(), Some("9+"));
        assert_eq!(label(250).as_deref(), Some("9+"));
    }

    fn image_rect() -> NSRect {
        NSRect::new(NSPoint::new(4.0, 1.0), NSSize::new(27.0, 22.0))
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn frame_sits_in_the_lower_right_of_the_icon_either_way_up() {
        let flipped = badge_frame(image_rect(), true);
        assert!(close(flipped.origin.x, 4.0 + 14.7));
        assert!(close(flipped.origin.y, 1.0 + 12.2));
        assert!(close(flipped.size.width, 12.2));

        // y up: the same spot measured from the other edge.
        let unflipped = badge_frame(image_rect(), false);
        assert!(close(unflipped.origin.x, flipped.origin.x));
        assert!(close(unflipped.origin.y, 1.0 + 22.0 - 24.4));
    }

    #[test]
    fn frame_scales_with_the_drawn_icon() {
        let half = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(13.5, 11.0));
        assert!(close(badge_frame(half, true).size.width, 6.1));
    }

    #[test]
    fn badged_icon_has_the_gap_where_the_badge_goes() {
        let icon = Image::from_bytes(BADGED_ICON_PNG).unwrap();
        let plain = Image::from_bytes(PLAIN_ICON_PNG).unwrap();
        assert_eq!(icon.height(), plain.height());

        let scale = f64::from(icon.width()) / GRID_WIDTH;
        let alpha = |x: f64, y: f64| {
            let (px, py) = ((x * scale) as usize, (y * scale) as usize);
            icon.rgba()[(py * icon.width() as usize + px) * 4 + 3]
        };
        assert_eq!(alpha(BADGE_CX, BADGE_CY), 0, "badge area is transparent");
        assert_eq!(alpha(15.0, 17.0), 0, "the gap bites into the head");
        assert_eq!(alpha(6.5, 17.0), 255, "the rest of the head stays");
    }

    fn token(css: &str, name: &str, from: usize) -> u32 {
        let at = css[from..].find(name).expect("token in app.css") + from;
        let hex = css[at + name.len()..].trim_start_matches([':', ' ', '#']);
        u32::from_str_radix(&hex[..6], 16).expect("a six-digit hex colour")
    }

    #[test]
    fn badge_matches_app_css_terracotta() {
        let css = include_str!("../../src/app.css");
        let dark = css.find("prefers-color-scheme: dark").expect("dark block");
        assert_eq!(LIGHT_FILL, token(css, "--terracotta:", 0));
        assert_eq!(DARK_FILL, token(css, "--terracotta:", dark));
    }
}
