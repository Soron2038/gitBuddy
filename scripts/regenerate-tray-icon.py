#!/usr/bin/env python3
"""
Regenerate the macOS tray icon as a 44×44 RGBA PNG.

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
from PIL import Image, ImageDraw

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

OUT = Path(__file__).resolve().parent.parent / "src-tauri" / "icons" / "tray-icon.png"


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


def stroke_polyline(draw: ImageDraw.ImageDraw, pts, width):
    """Polyline with round caps. `ImageDraw.line(joint='curve')` smooths
    the joints; explicit endcap ellipses fake `stroke-linecap=round`
    since Pillow doesn't expose that directly."""
    draw.line(pts, fill=BLACK, width=int(round(width)), joint="curve")
    r = width / 2
    for cx, cy in (pts[0], pts[-1]):
        draw.ellipse((cx - r, cy - r, cx + r, cy + r), fill=BLACK)


def main() -> None:
    img = Image.new("RGBA", (CANVAS, CANVAS), TRANSPARENT)
    draw = ImageDraw.Draw(img)

    # ── Antenna left branch ───────────────────────────────────────────
    # Straight stem from (11,10) up to (11,7.8), then a quadratic curve
    # to the left node anchor at (9.4,6.0) via control (11,6.5).
    left_stem = [(s(11), s(10)), (s(11), s(7.8))]
    left_curve = quadratic_bezier(
        (s(11), s(7.8)),
        (s(11), s(6.5)),
        (s(9.4), s(6.0)),
    )
    stroke_polyline(draw, left_stem + left_curve[1:], width=s(1.2))

    # ── Antenna right branch ──────────────────────────────────────────
    # Quadratic from (11,7.8) via control (12.3,7.8) to right node
    # anchor (13.0,6.6). Shares the start point with the left stem.
    right_curve = quadratic_bezier(
        (s(11), s(7.8)),
        (s(12.3), s(7.8)),
        (s(13.0), s(6.6)),
    )
    stroke_polyline(draw, right_curve, width=s(1.2))

    # ── Antenna nodes ────────────────────────────────────────────────
    for cx, cy, r in [(9.2, 5.0, 1.1), (13.2, 5.4, 1.1)]:
        draw.ellipse(
            (s(cx - r), s(cy - r), s(cx + r), s(cy + r)),
            fill=BLACK,
        )

    # ── Robot head with face punched out ─────────────────────────────
    # Build the head as its own RGBA layer, generate a mask image where
    # eyes + smile are black (= transparent on apply), then putalpha so
    # the cutouts knock through the head's fill.
    head = Image.new("RGBA", (CANVAS, CANVAS), TRANSPARENT)
    head_draw = ImageDraw.Draw(head)
    head_draw.rounded_rectangle(
        (s(5), s(10), s(17), s(19)),
        radius=s(2.5),
        fill=BLACK,
    )

    mask = Image.new("L", (CANVAS, CANVAS), 0)
    mask_draw = ImageDraw.Draw(mask)
    mask_draw.rounded_rectangle(
        (s(5), s(10), s(17), s(19)),
        radius=s(2.5),
        fill=255,
    )
    for cx, cy, rx, ry in [(8.5, 13.8, 0.9, 1.4), (13.5, 13.8, 0.9, 1.4)]:
        mask_draw.ellipse(
            (s(cx - rx), s(cy - ry), s(cx + rx), s(cy + ry)),
            fill=0,
        )
    smile_pts = quadratic_bezier(
        (s(9.7), s(16.2)),
        (s(11), s(17.2)),
        (s(12.3), s(16.2)),
    )
    mask_draw.line(smile_pts, fill=0, width=int(round(s(0.8))), joint="curve")
    smile_r = s(0.8) / 2
    for cx, cy in (smile_pts[0], smile_pts[-1]):
        mask_draw.ellipse(
            (cx - smile_r, cy - smile_r, cx + smile_r, cy + smile_r),
            fill=0,
        )

    head.putalpha(mask)
    img.alpha_composite(head)

    img = img.resize((SIZE, SIZE), Image.LANCZOS)
    img.save(OUT, "PNG", optimize=True)
    # Absolute path on purpose: relative_to(Path.cwd()) raises ValueError
    # when the script is run from anywhere but the repo root.
    print(f"wrote {OUT} ({SIZE}×{SIZE}, RGBA)")


if __name__ == "__main__":
    main()
