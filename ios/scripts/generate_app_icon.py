#!/usr/bin/env python3
"""Generate the app icon and launch mark from the bundled Instrument Serif Italic ampersand.

Requires fonttools, pillow and numpy. Output is deterministic and committed under
ios/AppAssembly/Assets.xcassets; rerun this script to regenerate it.
"""
import json
from pathlib import Path

import numpy as np
from fontTools.pens.basePen import BasePen
from fontTools.ttLib import TTFont
from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parents[2]
FONT_PATH = ROOT / "app/design-system/Fonts/InstrumentSerif-Italic.ttf"
CATALOG = ROOT / "ios/AppAssembly/Assets.xcassets"

ICON_SIZE = 1024
MARK_HEIGHT_FRACTION = 0.60
SUPERSAMPLE = 4
LAUNCH_MARK_SIZE = 512
CURVE_STEPS = 24

BRAND = (0x1E, 0x27, 0x61)
DARK_GROUND = (0x0A, 0x0B, 0x12)
DARK_MARK = (0x8E, 0x9B, 0xFF)
WHITE = (255, 255, 255)


class FlatteningPen(BasePen):
    """Collects each contour as a polyline, flattening quadratic and cubic curves."""

    def __init__(self, glyph_set):
        super().__init__(glyph_set)
        self.contours = []
        self._current = []

    def _moveTo(self, pt):
        self._current = [pt]

    def _lineTo(self, pt):
        self._current.append(pt)

    def _curveToOne(self, p1, p2, p3):
        p0 = self._getCurrentPoint()
        for step in range(1, CURVE_STEPS + 1):
            t = step / CURVE_STEPS
            u = 1 - t
            self._current.append((
                u**3 * p0[0] + 3 * u**2 * t * p1[0] + 3 * u * t**2 * p2[0] + t**3 * p3[0],
                u**3 * p0[1] + 3 * u**2 * t * p1[1] + 3 * u * t**2 * p2[1] + t**3 * p3[1],
            ))

    def _qCurveToOne(self, p1, p2):
        p0 = self._getCurrentPoint()
        for step in range(1, CURVE_STEPS + 1):
            t = step / CURVE_STEPS
            u = 1 - t
            self._current.append((
                u**2 * p0[0] + 2 * u * t * p1[0] + t**2 * p2[0],
                u**2 * p0[1] + 2 * u * t * p1[1] + t**2 * p2[1],
            ))

    def _closePath(self):
        if self._current:
            self.contours.append(self._current)
        self._current = []

    _endPath = _closePath


def ampersand_contours():
    font = TTFont(FONT_PATH)
    glyph_set = font.getGlyphSet()
    pen = FlatteningPen(glyph_set)
    glyph_set[font.getBestCmap()[ord("&")]].draw(pen)
    return pen.contours


def render_mark_mask(canvas_size, mark_height):
    """Return a float coverage mask (0..1) of the mark centred on a square canvas."""
    contours = ampersand_contours()
    points = [point for contour in contours for point in contour]
    min_x, max_x = min(p[0] for p in points), max(p[0] for p in points)
    min_y, max_y = min(p[1] for p in points), max(p[1] for p in points)
    scale = mark_height * SUPERSAMPLE / (max_y - min_y)
    big = canvas_size * SUPERSAMPLE
    offset_x = (big - (max_x - min_x) * scale) / 2
    offset_y = (big - (max_y - min_y) * scale) / 2
    parity = np.zeros((big, big), dtype=np.uint8)
    for contour in contours:
        layer = Image.new("1", (big, big), 0)
        ImageDraw.Draw(layer).polygon(
            [(offset_x + (x - min_x) * scale, offset_y + (max_y - y) * scale) for x, y in contour],
            fill=1,
        )
        parity ^= np.asarray(layer, dtype=np.uint8)  # even-odd fill keeps the counters open
    mask = Image.fromarray(parity * 255, "L").resize((canvas_size, canvas_size), Image.LANCZOS)
    return np.asarray(mask, dtype=np.float64) / 255.0


def composite(ground, mark_colour, mask):
    mark = np.array(mark_colour, dtype=np.float64)
    out = ground * (1 - mask[..., None]) + mark * mask[..., None]
    return Image.fromarray(np.clip(np.rint(out), 0, 255).astype(np.uint8), "RGB")


def solid_ground(colour):
    return np.broadcast_to(np.array(colour, dtype=np.float64), (ICON_SIZE, ICON_SIZE, 3)).copy()


def soft_top_light():
    """Brand ground with a soft white radial light centred above the top edge."""
    ys, xs = np.mgrid[0:ICON_SIZE, 0:ICON_SIZE].astype(np.float64)
    distance = np.hypot(xs - ICON_SIZE / 2, ys + ICON_SIZE * 0.1) / (ICON_SIZE * 0.95)
    strength = 0.22 * np.clip(1 - distance, 0, 1) ** 2
    ground = solid_ground(BRAND)
    return ground + (255 - ground) * strength[..., None]


def write_json(path, payload):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n")


def color_component(rgb):
    red, green, blue = (f"0x{channel:02X}" for channel in rgb)
    return {"color-space": "srgb", "components": {"alpha": "1.000", "red": red, "green": green, "blue": blue}}


def save_png(image, path):
    path.parent.mkdir(parents=True, exist_ok=True)
    image.save(path, format="PNG", optimize=False, compress_level=9)


def main():
    info = {"author": "xcode", "version": 1}
    write_json(CATALOG / "Contents.json", {"info": info})

    icon_mask = render_mark_mask(ICON_SIZE, ICON_SIZE * MARK_HEIGHT_FRACTION)
    icon_dir = CATALOG / "AppIcon.appiconset"
    save_png(composite(soft_top_light(), WHITE, icon_mask), icon_dir / "icon-default.png")
    save_png(composite(solid_ground(DARK_GROUND), DARK_MARK, icon_mask), icon_dir / "icon-dark.png")
    tinted = composite(solid_ground((0, 0, 0)), WHITE, icon_mask).convert("L").convert("RGB")
    save_png(tinted, icon_dir / "icon-tinted.png")
    write_json(icon_dir / "Contents.json", {
        "images": [
            {"filename": "icon-default.png", "idiom": "universal", "platform": "ios", "size": "1024x1024"},
            {"appearances": [{"appearance": "luminosity", "value": "dark"}],
             "filename": "icon-dark.png", "idiom": "universal", "platform": "ios", "size": "1024x1024"},
            {"appearances": [{"appearance": "luminosity", "value": "tinted"}],
             "filename": "icon-tinted.png", "idiom": "universal", "platform": "ios", "size": "1024x1024"},
        ],
        "info": info,
    })

    launch_mask = render_mark_mask(LAUNCH_MARK_SIZE, LAUNCH_MARK_SIZE * 0.8)
    alpha = Image.fromarray(np.rint(launch_mask * 255).astype(np.uint8), "L")
    launch_mark = Image.new("RGBA", alpha.size, WHITE + (0,))
    launch_mark.putalpha(alpha)
    mark_dir = CATALOG / "LaunchMark.imageset"
    save_png(launch_mark, mark_dir / "launch-mark.png")
    write_json(mark_dir / "Contents.json", {
        "images": [{"filename": "launch-mark.png", "idiom": "universal", "scale": "3x"}],
        "info": info,
    })

    write_json(CATALOG / "LaunchBackground.colorset/Contents.json", {
        "colors": [
            {"color": color_component(BRAND), "idiom": "universal"},
            {"appearances": [{"appearance": "luminosity", "value": "dark"}],
             "color": color_component(DARK_GROUND), "idiom": "universal"},
        ],
        "info": info,
    })


if __name__ == "__main__":
    main()
