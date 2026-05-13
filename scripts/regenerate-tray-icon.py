#!/usr/bin/env python3
"""
Regenerate the macOS tray icon as a 44×44 RGBA PNG.

The PNG is loaded at runtime by `include_bytes!` in src-tauri/src/lib.rs and
used as the menu-bar template image. macOS automatically tints the silhouette
to match the menu bar appearance, so the source has to be pure black-on-
transparent with crisp alpha edges — which is what this script produces by
drawing at 4× supersample and downsampling with Lanczos.

The matching tray-icon.svg next to this script is the design reference; this
script is the canonical renderer because Tauri eats PNG and macOS doesn't
ship a reliable system-level SVG-to-PNG converter.

Usage:
    python3 scripts/regenerate-tray-icon.py

Requires Pillow:
    pip3 install --user pillow
"""

from pathlib import Path
from PIL import Image, ImageDraw

SIZE = 44
SUPERSAMPLE = 4
CANVAS = SIZE * SUPERSAMPLE
BLACK = (0, 0, 0, 255)

OUT = Path(__file__).resolve().parent.parent / "src-tauri" / "icons" / "tray-icon.png"


def s(v: float) -> int:
    """Scale a design-unit coordinate (in the 44-unit grid) to the supersampled canvas."""
    return int(v * SUPERSAMPLE)


def main() -> None:
    img = Image.new("RGBA", (CANVAS, CANVAS), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)

    # Body — round, fills the lower half of the canvas.
    draw.ellipse((s(10), s(15), s(34), s(39)), fill=BLACK)

    # Leaves — 6-vertex polygons that overlap the body at the base so there's
    # no visible seam. Each leaf goes: body → up-inner → up → tip → down-outer
    # → back to body. The first and last vertices both touch the body.
    left_leaf = [(22, 16), (17, 11), (10, 7), (5, 5), (8, 12), (14, 17)]
    right_leaf = [(22, 16), (27, 11), (34, 7), (39, 5), (36, 12), (30, 17)]

    draw.polygon([(s(x), s(y)) for x, y in left_leaf], fill=BLACK)
    draw.polygon([(s(x), s(y)) for x, y in right_leaf], fill=BLACK)

    # Downsample with anti-aliasing for crisp curved edges at 44×44.
    img = img.resize((SIZE, SIZE), Image.LANCZOS)
    img.save(OUT, "PNG", optimize=True)
    print(f"wrote {OUT.relative_to(Path.cwd())} ({SIZE}×{SIZE}, RGBA)")


if __name__ == "__main__":
    main()
