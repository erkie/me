#!/usr/bin/env python3
"""Draws the chosen logo, Selected on paper with a red caret, into assets/logo.

    app-icon.svg, app-icon-dark.svg   macOS app icons, light and dark
    wordmark.svg, wordmark-dark.svg   the wordmark for light and dark backgrounds
    logo-mark.svg                     one-colour mark the app tints (empty editor)
    AppIcon.icon/                     the macOS 26 icon, in Icon Composer's format

Run script/render-icons afterwards for the PNGs. Same requirements as
generate.py.

    python3 assets/logo/explorations/final.py
"""

import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from generate import HERE, Run, f, selected  # noqa: E402
from selected_colors import icon  # noqa: E402

# Background, selection, ink, caret. The selection blue is the one macOS uses
# for selected text; the reds are Apple's system red in light and dark mode.
# Dark keeps the same selected word and only darkens what's around it.
LIGHT = ("#FAF7F0", "#B3D7FF", "#1D1D1F", "#FF3B30")
DARK = ("#1C1B19", "#B3D7FF", "#1D1D1F", "#FF453A")


def mark(colors, slug):
    bg, sel, ink, caret = colors
    return selected(ink, sel, caret, bg, slug, slug, ("Ink", "Selection", "Caret", "Background"))


def logo_mark():
    """The wordmark in one colour: the selection with "me" cut out of it, and
    the caret. The app draws it as a mask in its own text colour."""
    r = Run("bricolage-xb", "me", 200, tracking=-0.02)
    x0, _, x1, _ = r.box()
    top, bottom = -r.xh - 50, 46
    caret_x = x1 + 22
    sx0 = x0 - 26
    gap = 14  # one colour: the caret needs air to read apart from the selection
    box = (sx0, top - 14, caret_x + gap + 17, bottom + 14)
    # A square viewBox, so the mark sits centred in the square the app gives it.
    w, h = box[2] - box[0], box[3] - box[1]
    side = w * 1.04
    vx, vy = box[0] - (side - w) / 2, box[1] - (side - h) / 2
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="{f(vx)} {f(vy)} {f(side)} {f(side)}" '
        f'width="64" height="64">'
        f'<mask id="me-letters" maskUnits="userSpaceOnUse" x="{f(vx)}" y="{f(vy)}" width="{f(side)}" height="{f(side)}">'
        f'<rect x="{f(vx)}" y="{f(vy)}" width="{f(side)}" height="{f(side)}" fill="#fff"/>'
        f'<path d="{r.d()}" fill="#000"/></mask>'
        f'<rect x="{f(sx0)}" y="{f(top)}" width="{f(caret_x - sx0)}" height="{f(bottom - top)}" rx="18" '
        f'fill="#000" mask="url(#me-letters)"/>'
        f'<rect x="{f(caret_x + gap)}" y="{f(top - 14)}" width="17" height="{f(bottom - top + 28)}" rx="8.5" fill="#000"/>'
        "</svg>\n"
    )


def icon_composer(light, dark, path):
    """The icon in Icon Composer's format, which Xcode 26's actool compiles
    into the light, dark, clear and tinted icons macOS 26 switches between.
    The background is the icon's fill; the selected word is a layer, swapped
    for its dark-mode colours in dark mode. The canvas is the whole icon
    shape, so the word takes the same share of it as on app-icon.svg."""
    def layer(mark):
        x0, y0, x1, y1 = mark.box
        scale = min(1024 * 0.66 / (x1 - x0), 1024 * 0.5 / (y1 - y0))
        return (
            '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024" width="1024" height="1024">'
            f'<g transform="translate(512 512) scale({scale:.4f}) translate({f(-(x0 + x1) / 2)} {f(-(y0 + y1) / 2)})">'
            f"{mark.markup(mark.slug + '-layer')}</g></svg>\n"
        )

    def color(hex_color):
        r, g, b = (int(hex_color[i:i + 2], 16) / 255 for i in (1, 3, 5))
        return f"srgb:{r:.5f},{g:.5f},{b:.5f},1.00000"

    os.makedirs(os.path.join(path, "Assets"), exist_ok=True)
    for name, m in (("selected.svg", light), ("selected-dark.svg", dark)):
        with open(os.path.join(path, "Assets", name), "w") as fh:
            fh.write(layer(m))
    flat = {"glass": False}
    manifest = {
        "fill-specializations": [
            {"value": {"solid": color(light.bg)}},
            {"appearance": "dark", "value": {"solid": color(dark.bg)}},
        ],
        "groups": [{
            "name": "Selected",
            "layers": [
                {"name": "selected", "image-name": "selected.svg", **flat,
                 "hidden-specializations": [{"value": False}, {"appearance": "dark", "value": True}]},
                {"name": "selected-dark", "image-name": "selected-dark.svg", **flat,
                 "hidden-specializations": [{"value": True}, {"appearance": "dark", "value": False}]},
            ],
            "shadow": {"kind": "neutral", "opacity": 0.5},
            "specular": False,
            "translucency": {"enabled": False, "value": 0.5},
        }],
        "supported-platforms": {"squares": "shared"},
    }
    with open(os.path.join(path, "icon.json"), "w") as fh:
        json.dump(manifest, fh, indent=2)
        fh.write("\n")


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else os.path.dirname(HERE)
    light, dark = mark(LIGHT, "me-light"), mark(DARK, "me-dark")
    files = {
        "app-icon.svg": icon(light).svg(),
        "app-icon-dark.svg": icon(dark).svg(),
        "wordmark.svg": light.wordmark_svg(),
        "wordmark-dark.svg": dark.wordmark_svg(),
        "logo-mark.svg": logo_mark(),
    }
    for name, svg in files.items():
        with open(os.path.join(out, name), "w") as fh:
            fh.write(svg)
        print(name)
    icon_composer(light, dark, os.path.join(out, "AppIcon.icon"))
    print("AppIcon.icon/")


if __name__ == "__main__":
    main()
