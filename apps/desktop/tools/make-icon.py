#!/usr/bin/env python3
"""Draw the Orama app icon.

This script is the icon's source. There is no .svg and no binary master: the
mark is two circles and a rounded square, which is less code to keep than an
asset nobody can diff. Run it, then hand the PNG to `tauri icon`, which emits
every size and format the three bundlers need:

    python3 tools/make-icon.py
    npm run icon

Colours are taken from the dashboard's own stylesheet
(`apps/web/src/styles/index.css`) and converted from OKLCH here, so the app
icon and the page it opens cannot drift apart by hand-picked hex.

Requires Pillow. Only needed to regenerate the icon, never to build the app —
the generated PNGs are committed.
"""

import math
from PIL import Image, ImageDraw

# --- palette, lifted from apps/web/src/styles/index.css ---------------------
SURFACE = (0.19, 0.006, 265)  # --color-surface
BG = (0.145, 0.006, 265)  # a touch below --color-bg, for the gradient floor
ACCENT = (0.68, 0.17, 268)  # --color-accent
ACCENT_DIM = (0.42, 0.11, 268)  # between accent and --color-accent-soft

# Drawn large and downsampled: Pillow has no anti-aliased shape rasteriser, so
# supersampling is what keeps the ring's edge from looking chewed.
SIZE = 1024
SCALE = 4
CANVAS = SIZE * SCALE


def oklch_to_rgb(lightness: float, chroma: float, hue_deg: float) -> tuple[int, int, int]:
    """OKLCH to 8-bit sRGB, clipped to gamut."""
    hue = math.radians(hue_deg)
    a = chroma * math.cos(hue)
    b = chroma * math.sin(hue)

    l_ = lightness + 0.3963377774 * a + 0.2158037573 * b
    m_ = lightness - 0.1055613458 * a - 0.0638541728 * b
    s_ = lightness - 0.0894841775 * a - 1.2914855480 * b
    l, m, s = l_**3, m_**3, s_**3

    linear = (
        +4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
        -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
        -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s,
    )

    def encode(channel: float) -> int:
        channel = max(0.0, min(1.0, channel))
        srgb = 12.92 * channel if channel <= 0.0031308 else 1.055 * channel ** (1 / 2.4) - 0.055
        return round(max(0.0, min(1.0, srgb)) * 255)

    return tuple(encode(c) for c in linear)


def main() -> None:
    surface = oklch_to_rgb(*SURFACE)
    floor = oklch_to_rgb(*BG)
    accent = oklch_to_rgb(*ACCENT)
    accent_dim = oklch_to_rgb(*ACCENT_DIM)

    image = Image.new("RGBA", (CANVAS, CANVAS), (0, 0, 0, 0))
    draw = ImageDraw.Draw(image)

    # Vertical gradient body, so the tile has some depth at large sizes and
    # still flattens to a solid dark square in a 16px tab strip.
    gradient = Image.new("RGB", (1, CANVAS))
    for y in range(CANVAS):
        t = y / (CANVAS - 1)
        gradient.putpixel(
            (0, y),
            tuple(round(surface[c] + (floor[c] - surface[c]) * t) for c in range(3)),
        )
    gradient = gradient.resize((CANVAS, CANVAS))

    # Rounded-square mask. The inset leaves the breathing room macOS expects
    # around an app icon; the radius is the usual ~22% of the tile.
    inset = round(CANVAS * 0.055)
    radius = round(CANVAS * 0.22)
    mask = Image.new("L", (CANVAS, CANVAS), 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        (inset, inset, CANVAS - inset, CANVAS - inset), radius=radius, fill=255
    )
    image.paste(gradient, (0, 0), mask)

    # The mark: an aperture. A ring you look through, with the pupil closed on
    # a point — the tool watches one stream and resolves it to one thing.
    centre = CANVAS / 2
    ring_radius = CANVAS * 0.255
    ring_width = round(CANVAS * 0.072)

    # Traffic passing through the lens: two short rules that stop at the ring,
    # so the mark reads as something in the path rather than a plain target.
    rule_y = centre
    rule_half = round(ring_width * 0.36)
    for x0, x1 in (
        (inset + CANVAS * 0.022, centre - ring_radius - CANVAS * 0.05),
        (centre + ring_radius + CANVAS * 0.05, CANVAS - inset - CANVAS * 0.022),
    ):
        draw.rounded_rectangle(
            (x0, rule_y - rule_half, x1, rule_y + rule_half),
            radius=rule_half,
            fill=accent_dim,
        )

    draw.ellipse(
        (
            centre - ring_radius,
            centre - ring_radius,
            centre + ring_radius,
            centre + ring_radius,
        ),
        outline=accent,
        width=ring_width,
    )

    pupil = CANVAS * 0.088
    draw.ellipse(
        (centre - pupil, centre - pupil, centre + pupil, centre + pupil),
        fill=accent,
    )

    image.resize((SIZE, SIZE), Image.LANCZOS).save("app-icon.png")
    print(f"wrote app-icon.png ({SIZE}x{SIZE})")


if __name__ == "__main__":
    main()
