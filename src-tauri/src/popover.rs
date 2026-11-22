//! Where the popover opens, and the guard against the tray click that both
//! closes and reopens it. Pure functions over physical pixels, so the tests
//! run on every platform even though only Windows uses the placement today.
//!
//! On macOS the popover always drops down from the menu bar, which
//! `toggle_popover` in `lib.rs` still computes inline, exactly as before. On
//! Windows the taskbar can sit on any edge, and usually at the bottom, where
//! "below the icon" is off-screen. So the popover opens towards the inside of
//! the screen, from whichever edge the taskbar is on, and is clamped into the
//! monitor's work area (the monitor minus the taskbar).

// macOS only uses the reopen guard; the geometry is for everything else.
#![cfg_attr(target_os = "macos", allow(dead_code))]

use std::time::{Duration, Instant};

/// A rectangle in physical pixels, in virtual-screen coordinates: a monitor
/// left of or above the primary one has negative coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PxRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl PxRect {
    pub(crate) fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    fn right(self) -> i32 {
        self.x.saturating_add_unsigned(self.width)
    }

    fn bottom(self) -> i32 {
        self.y.saturating_add_unsigned(self.height)
    }

    pub(crate) fn center(self) -> (i32, i32) {
        (
            self.x.saturating_add_unsigned(self.width / 2),
            self.y.saturating_add_unsigned(self.height / 2),
        )
    }

    pub(crate) fn contains(self, x: i32, y: i32) -> bool {
        (self.x..self.right()).contains(&x) && (self.y..self.bottom()).contains(&y)
    }
}

/// The screen edge the taskbar is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Edge {
    Top,
    Bottom,
    Left,
    Right,
}

/// The taskbar's edge is the one the work area is inset from. An auto-hiding
/// taskbar doesn't inset the work area at all; then it's the monitor edge
/// nearest the tray icon, which sits on the taskbar while it is clicked.
pub(crate) fn taskbar_edge(tray: PxRect, monitor: PxRect, work: PxRect) -> Edge {
    let insets = [
        (Edge::Bottom, monitor.bottom() - work.bottom()),
        (Edge::Top, work.y - monitor.y),
        (Edge::Right, monitor.right() - work.right()),
        (Edge::Left, work.x - monitor.x),
    ];
    let mut widest = (Edge::Bottom, 0);
    for (edge, inset) in insets {
        if inset > widest.1 {
            widest = (edge, inset);
        }
    }
    if widest.1 > 0 {
        return widest.0;
    }

    let (cx, cy) = tray.center();
    let distances = [
        (Edge::Bottom, monitor.bottom() - cy),
        (Edge::Top, cy - monitor.y),
        (Edge::Right, monitor.right() - cx),
        (Edge::Left, cx - monitor.x),
    ];
    let mut nearest = distances[0];
    for candidate in distances {
        if candidate.1 < nearest.1 {
            nearest = candidate;
        }
    }
    nearest.0
}

/// Top-left corner of a `pop`-sized popover for a click on `tray`.
///
/// The popover opens away from the taskbar edge, `gap` pixels clear of both
/// the taskbar and the icon (the icon can sit above the taskbar, in the
/// overflow flyout), centred on the icon along the other axis, then clamped
/// into the work area with the same `gap` as margin. A popover larger than
/// the work area is pinned to its top-left corner instead of panicking.
pub(crate) fn popover_origin(
    tray: PxRect,
    pop: (u32, u32),
    monitor: PxRect,
    work: PxRect,
    gap: i32,
) -> (i32, i32) {
    let (pop_w, pop_h) = (
        i32::try_from(pop.0).unwrap_or(i32::MAX),
        i32::try_from(pop.1).unwrap_or(i32::MAX),
    );
    let (cx, cy) = tray.center();
    let centred_x = cx - pop_w / 2;
    let centred_y = cy - pop_h / 2;

    let (x, y) = match taskbar_edge(tray, monitor, work) {
        Edge::Bottom => (centred_x, tray.y.min(work.bottom()) - gap - pop_h),
        Edge::Top => (centred_x, tray.bottom().max(work.y) + gap),
        Edge::Right => (tray.x.min(work.right()) - gap - pop_w, centred_y),
        Edge::Left => (tray.right().max(work.x) + gap, centred_y),
    };

    // Not `i32::clamp`: it panics when the popover doesn't fit (hi < lo).
    let fit = |v: i32, lo: i32, hi: i32| v.min(hi).max(lo);
    (
        fit(x, work.x + gap, work.right() - pop_w - gap),
        fit(y, work.y + gap, work.bottom() - pop_h - gap),
    )
}

/// How long after the popover hid itself on focus loss a tray click counts as
/// the click that took the focus. Clicking the tray icon focuses the taskbar
/// first, on mouse-down, so the popover hides; the click event only arrives
/// on mouse-up and would show it again. Within this window it doesn't.
pub(crate) const REOPEN_GUARD: Duration = Duration::from_millis(500);

/// Whether a tray click at `now` should leave the popover closed, because it
/// is the click whose mouse-down just closed it (see [`REOPEN_GUARD`]).
pub(crate) fn closed_by_this_click(auto_hidden_at: Option<Instant>, now: Instant) -> bool {
    auto_hidden_at.is_some_and(|at| now.saturating_duration_since(at) < REOPEN_GUARD)
}

#[cfg(test)]
mod tests {
    use super::*;

    const POP: (u32, u32) = (440, 620);
    const GAP: i32 = 12;

    /// A 1920×1080 monitor at `(x, y)` with a 48-px taskbar on `edge`:
    /// returns the monitor and its work area.
    fn screen(x: i32, y: i32, edge: Edge) -> (PxRect, PxRect) {
        let monitor = PxRect::new(x, y, 1920, 1080);
        let work = match edge {
            Edge::Bottom => PxRect::new(x, y, 1920, 1032),
            Edge::Top => PxRect::new(x, y + 48, 1920, 1032),
            Edge::Left => PxRect::new(x + 48, y, 1872, 1080),
            Edge::Right => PxRect::new(x, y, 1872, 1080),
        };
        (monitor, work)
    }

    #[test]
    fn bottom_taskbar_opens_upwards_above_the_taskbar() {
        let (monitor, work) = screen(0, 0, Edge::Bottom);
        let tray = PxRect::new(1600, 1036, 24, 40);
        assert_eq!(taskbar_edge(tray, monitor, work), Edge::Bottom);
        // Centred on the icon: 1612 - 220 = 1392; bottom 12 px above the
        // taskbar's top edge at 1032.
        assert_eq!(
            popover_origin(tray, POP, monitor, work, GAP),
            (1392, 1032 - GAP - 620)
        );
    }

    #[test]
    fn an_icon_near_the_corner_is_clamped_into_the_work_area() {
        let (monitor, work) = screen(0, 0, Edge::Bottom);
        let tray = PxRect::new(1880, 1036, 24, 40);
        let (x, _) = popover_origin(tray, POP, monitor, work, GAP);
        assert_eq!(x, 1920 - 440 - GAP);
    }

    #[test]
    fn top_taskbar_opens_downwards() {
        let (monitor, work) = screen(0, 0, Edge::Top);
        let tray = PxRect::new(1600, 4, 24, 40);
        assert_eq!(taskbar_edge(tray, monitor, work), Edge::Top);
        assert_eq!(
            popover_origin(tray, POP, monitor, work, GAP),
            (1392, 48 + GAP)
        );
    }

    #[test]
    fn side_taskbars_open_sideways() {
        let (monitor, work) = screen(0, 0, Edge::Left);
        let tray = PxRect::new(4, 900, 40, 24);
        assert_eq!(taskbar_edge(tray, monitor, work), Edge::Left);
        // Centred on y = 912 would end at 1222; clamped to 1080 - 620 - 12.
        assert_eq!(
            popover_origin(tray, POP, monitor, work, GAP),
            (48 + GAP, 1080 - 620 - GAP)
        );

        let (monitor, work) = screen(0, 0, Edge::Right);
        let tray = PxRect::new(1876, 500, 40, 24);
        assert_eq!(taskbar_edge(tray, monitor, work), Edge::Right);
        assert_eq!(
            popover_origin(tray, POP, monitor, work, GAP),
            (1872 - GAP - 440, 512 - 310)
        );
    }

    #[test]
    fn an_auto_hiding_taskbar_is_found_from_the_icon() {
        // No inset: the work area is the whole monitor.
        let monitor = PxRect::new(0, 0, 1920, 1080);
        let tray = PxRect::new(1600, 1036, 24, 40);
        assert_eq!(taskbar_edge(tray, monitor, monitor), Edge::Bottom);
        // Above the icon, since the shown taskbar overlaps the work area.
        assert_eq!(
            popover_origin(tray, POP, monitor, monitor, GAP),
            (1392, 1036 - GAP - 620)
        );

        let tray = PxRect::new(1600, 4, 24, 40);
        assert_eq!(taskbar_edge(tray, monitor, monitor), Edge::Top);
    }

    #[test]
    fn an_icon_in_the_overflow_flyout_gets_a_popover_above_it() {
        // The flyout sits above the taskbar, so the icon is inside the work
        // area; the popover clears the icon rather than the taskbar.
        let (monitor, work) = screen(0, 0, Edge::Bottom);
        let tray = PxRect::new(1500, 950, 32, 32);
        assert_eq!(
            popover_origin(tray, POP, monitor, work, GAP),
            (1516 - 220, 950 - GAP - 620)
        );
    }

    #[test]
    fn a_monitor_left_of_the_primary_has_negative_coordinates() {
        let (monitor, work) = screen(-1920, 0, Edge::Bottom);
        let tray = PxRect::new(-320, 1036, 24, 40);
        assert_eq!(taskbar_edge(tray, monitor, work), Edge::Bottom);
        assert_eq!(
            popover_origin(tray, POP, monitor, work, GAP),
            (-308 - 220, 1032 - GAP - 620)
        );
        // Clamped to this monitor's work area, not the primary's.
        let tray = PxRect::new(-1916, 1036, 24, 40);
        assert_eq!(popover_origin(tray, POP, monitor, work, GAP).0, -1920 + GAP);
    }

    #[test]
    fn contains_is_half_open() {
        let r = PxRect::new(-10, 0, 20, 10);
        assert!(r.contains(-10, 0));
        assert!(r.contains(9, 9));
        assert!(!r.contains(10, 5));
        assert!(!r.contains(0, 10));
    }

    #[test]
    fn a_popover_larger_than_the_work_area_is_pinned_not_a_panic() {
        let monitor = PxRect::new(0, 0, 400, 300);
        let work = PxRect::new(0, 0, 400, 260);
        let tray = PxRect::new(380, 264, 16, 32);
        assert_eq!(popover_origin(tray, POP, monitor, work, GAP), (GAP, GAP));
    }

    #[test]
    fn the_click_that_closed_the_popover_does_not_reopen_it() {
        let now = Instant::now();
        let hid = now.checked_sub(Duration::from_millis(80)).unwrap_or(now);
        assert!(closed_by_this_click(Some(hid), now));
        let long_ago = now.checked_sub(Duration::from_secs(2)).unwrap_or(now);
        if long_ago != now {
            assert!(!closed_by_this_click(Some(long_ago), now));
        }
        assert!(!closed_by_this_click(None, now));
    }
}
