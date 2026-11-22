#!/usr/bin/env python3
"""
Regenerate the macOS tray icon as a 44×44 RGBA PNG, plus the wider variant
used while the count badge is showing, and the four 32×32 Windows
notification-area icons (see `write_windows` below).

The PNG is loaded at runtime by `include_bytes!` in src-tauri/src/lib.rs and
used as the menu-bar template image. macOS automatically tints the silhouette
to match the menu bar appearance, so the source has to be pure black-on-
transparent with crisp alpha edges — which is what this script produces by
drawing at 8× supersample and downsampling with Lanczos.

The matching src-tauri/icons/tray-icon.svg is the design reference; this
script is the canonical renderer because Tauri eats PNG and macOS doesn't
ship a reliable system-level SVG-to-PNG converter at small sizes.

Design (mirrors src-tauri/icons/tray-icon.svg):
  * Antenna = git-branch symbol: forked stem with two nodes
  * Robot head = rounded rectangle with eyes + smile punched out so the
    "face" reads at 22-px menu-bar sizes

Usage:
    python3 scripts/regenerate-tray-icon.py

Requires Pillow:
    pip3 install --user pillow
"""

from pathlib import Path
from PIL import Image, ImageChops, ImageDraw

# Output is a 44×44 PNG. On Retina it gets sampled to 22 logical pixels,
# hence the 22-unit design grid. Supersampling 8× gives smooth round caps
# and curved antenna joints after Lanczos downscale.
SIZE = 44
SUPERSAMPLE = 8
DESIGN_GRID = 22
CANVAS = SIZE * SUPERSAMPLE
SCALE = CANVAS / DESIGN_GRID

BLACK = (0, 0, 0, 255)
TRANSPARENT = (0, 0, 0, 0)

# The badged variant, in design units. Keep in sync with the constants in
# src-tauri/src/tray_badge.rs, which draws the badge at this position.
BADGED_GRID_WIDTH = 27
BADGE_CX = 20.8
BADGE_CY = 18.3
BADGE_R = 6.1
BADGE_GAP = 1

ICONS = Path(__file__).resolve().parent.parent / "src-tauri" / "icons"
OUT = ICONS / "tray-icon.png"
OUT_BADGED = ICONS / "tray-icon-badged.png"

# Windows doesn't tint notification-area icons, so there is one icon per
# taskbar theme. They are 32×32: Windows scales them to 16 px at 100 % and
# shows them as they are at 200 %. The view is a square window, in design
# units, cropped tighter around the robot than the macOS 22-unit grid, since
# a 16-px icon can't spare the margin. While clones are behind, a terracotta
# dot sits in a bite out of the lower right corner; digits don't read at
# 16 px, so the count stays in the tooltip. `tray_windows.rs` picks the file.
WIN_SIZE = 32
WIN_VIEW = (2.5, 2.95, 17.0)  # x, y of the top-left corner, side length
# As large as the corner allows without biting into the right eye or the
# smile, and flush with the view's right and bottom edges.
WIN_DOT_CX = 16.8
WIN_DOT_CY = 17.25
WIN_DOT_R = 2.7
WIN_DOT_GAP = 0.8
# Glyph per taskbar theme, and the dot in `--terracotta` of the matching
# scheme from src/app.css (the same pair tray_badge.rs uses on macOS).
WIN_VARIANTS = {
    "light": {"glyph": (0x1A, 0x1A, 0x1A, 255), "dot": (0xB5, 0x54, 0x36, 255)},
    "dark": {"glyph": (0xFF, 0xFF, 0xFF, 255), "dot": (0xED, 0x8F, 0x6A, 255)},
}


def s(v: float) -> float:
    """Scale a design-unit coordinate to the supersampled canvas."""
    return v * SCALE


def quadratic_bezier(p0, p1, p2, steps=40):
    """Sample a quadratic Bézier curve as a polyline."""
    pts = []
    for i in range(steps + 1):
        t = i / steps
        u = 1 - t
        x = u * u * p0[0] + 2 * u * t * p1[0] + t * t * p2[0]
        y = u * u * p0[1] + 2 * u * t * p1[1] + t * t * p2[1]
        pts.append((x, y))
    return pts


def stroke_polyline(draw: ImageDraw.ImageDraw, pts, width, colour=BLACK):
    """Polyline with round caps. `ImageDraw.line(joint='curve')` smooths
    the joints; explicit endcap ellipses fake `stroke-linecap=round`
    since Pillow doesn't expose that directly."""
    draw.line(pts, fill=colour, width=int(round(width)), joint="curve")
    r = width / 2
    for cx, cy in (pts[0], pts[-1]):
        draw.ellipse((cx - r, cy - r, cx + r, cy + r), fill=colour)


def draw_robot(canvas: int, view=(0.0, 0.0, DESIGN_GRID), colour=BLACK) -> Image.Image:
    """The robot on a square `canvas`-px RGBA image showing the design-unit
    square `view` (left, top, side). The default view is the macOS 22-unit
    grid, for which `px`/`py` reduce exactly to `s()`."""
    vx, vy, vsize = view
    k = canvas / vsize

    def px(v: float) -> float:
        return (v - vx) * k

    def py(v: float) -> float:
        return (v - vy) * k

    def d(v: float) -> float:
        return v * k

    img = Image.new("RGBA", (canvas, canvas), TRANSPARENT)
    draw = ImageDraw.Draw(img)

    # ── Antenna left branch ───────────────────────────────────────────
    # Straight stem from (11,10) up to (11,7.8), then a quadratic curve
    # to the left node anchor at (9.4,6.0) via control (11,6.5).
    left_stem = [(px(11), py(10)), (px(11), py(7.8))]
    left_curve = quadratic_bezier(
        (px(11), py(7.8)),
        (px(11), py(6.5)),
        (px(9.4), py(6.0)),
    )
    stroke_polyline(draw, left_stem + left_curve[1:], width=d(1.2), colour=colour)

    # ── Antenna right branch ──────────────────────────────────────────
    # Quadratic from (11,7.8) via control (12.3,7.8) to right node
    # anchor (13.0,6.6). Shares the start point with the left stem.
    right_curve = quadratic_bezier(
        (px(11), py(7.8)),
        (px(12.3), py(7.8)),
        (px(13.0), py(6.6)),
    )
    stroke_polyline(draw, right_curve, width=d(1.2), colour=colour)

    # ── Antenna nodes ────────────────────────────────────────────────
    for cx, cy, r in [(9.2, 5.0, 1.1), (13.2, 5.4, 1.1)]:
        draw.ellipse(
            (px(cx - r), py(cy - r), px(cx + r), py(cy + r)),
            fill=colour,
        )

    # ── Robot head with face punched out ─────────────────────────────
    # Build the head as its own RGBA layer, generate a mask image where
    # eyes + smile are black (= transparent on apply), then putalpha so
    # the cutouts knock through the head's fill.
    head = Image.new("RGBA", (canvas, canvas), TRANSPARENT)
    head_draw = ImageDraw.Draw(head)
    head_draw.rounded_rectangle(
        (px(5), py(10), px(17), py(19)),
        radius=d(2.5),
        fill=colour,
    )

    mask = Image.new("L", (canvas, canvas), 0)
    mask_draw = ImageDraw.Draw(mask)
    mask_draw.rounded_rectangle(
        (px(5), py(10), px(17), py(19)),
        radius=d(2.5),
        fill=255,
    )
    for cx, cy, rx, ry in [(8.5, 13.8, 0.9, 1.4), (13.5, 13.8, 0.9, 1.4)]:
        mask_draw.ellipse(
            (px(cx - rx), py(cy - ry), px(cx + rx), py(cy + ry)),
            fill=0,
        )
    smile_pts = quadratic_bezier(
        (px(9.7), py(16.2)),
        (px(11), py(17.2)),
        (px(12.3), py(16.2)),
    )
    mask_draw.line(smile_pts, fill=0, width=int(round(d(0.8))), joint="curve")
    smile_r = d(0.8) / 2
    for cx, cy in (smile_pts[0], smile_pts[-1]):
        mask_draw.ellipse(
            (cx - smile_r, cy - smile_r, cx + smile_r, cy + smile_r),
            fill=0,
        )

    head.putalpha(mask)
    img.alpha_composite(head)
    return img


def main() -> None:
    img = draw_robot(CANVAS)

    write_badged(img)
    write_windows()

    img = img.resize((SIZE, SIZE), Image.LANCZOS)
    img.save(OUT, "PNG", optimize=True)
    # Absolute path on purpose: relative_to(Path.cwd()) raises ValueError
    # when the script is run from anywhere but the repo root.
    print(f"wrote {OUT} ({SIZE}×{SIZE}, RGBA)")


def write_badged(robot: Image.Image) -> None:
    """The same robot on a wider canvas with a round bite out of its lower
    right corner. `tray_badge.rs` draws the coloured count into that bite;
    the robot itself stays a template image macOS tints on its own."""
    wide = Image.new("RGBA", (int(s(BADGED_GRID_WIDTH)), CANVAS), TRANSPARENT)
    wide.alpha_composite(robot)

    keep = Image.new("L", wide.size, 255)
    r = BADGE_R + BADGE_GAP
    ImageDraw.Draw(keep).ellipse(
        (s(BADGE_CX - r), s(BADGE_CY - r), s(BADGE_CX + r), s(BADGE_CY + r)),
        fill=0,
    )
    wide.putalpha(ImageChops.multiply(wide.getchannel("A"), keep))

    size = (BADGED_GRID_WIDTH * SIZE // DESIGN_GRID, SIZE)
    wide.resize(size, Image.LANCZOS).save(OUT_BADGED, "PNG", optimize=True)
    print(f"wrote {OUT_BADGED} ({size[0]}×{size[1]}, RGBA)")


def write_windows() -> None:
    """`tray-win-<theme>.png` and `tray-win-<theme>-badged.png` for a light
    and a dark taskbar. The glyph is filled with its final colour; Windows
    shows the PNG as it is."""
    canvas = WIN_SIZE * SUPERSAMPLE
    vx, vy, vsize = WIN_VIEW
    k = canvas / vsize

    def circle(r: float):
        cx, cy = (WIN_DOT_CX - vx) * k, (WIN_DOT_CY - vy) * k
        return (cx - r * k, cy - r * k, cx + r * k, cy + r * k)

    for theme, colours in WIN_VARIANTS.items():
        robot = draw_robot(canvas, WIN_VIEW, colours["glyph"])
        out = ICONS / f"tray-win-{theme}.png"
        robot.resize((WIN_SIZE, WIN_SIZE), Image.LANCZOS).save(out, "PNG", optimize=True)
        print(f"wrote {out} ({WIN_SIZE}×{WIN_SIZE}, RGBA)")

        keep = Image.new("L", robot.size, 255)
        ImageDraw.Draw(keep).ellipse(circle(WIN_DOT_R + WIN_DOT_GAP), fill=0)
        robot.putalpha(ImageChops.multiply(robot.getchannel("A"), keep))
        ImageDraw.Draw(robot).ellipse(circle(WIN_DOT_R), fill=colours["dot"])
        out = ICONS / f"tray-win-{theme}-badged.png"
        robot.resize((WIN_SIZE, WIN_SIZE), Image.LANCZOS).save(out, "PNG", optimize=True)
        print(f"wrote {out} ({WIN_SIZE}×{WIN_SIZE}, RGBA)")


if __name__ == "__main__":
    main()
