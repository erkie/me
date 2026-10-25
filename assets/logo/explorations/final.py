#!/usr/bin/env python3
"""Draws the chosen logo, Selected on paper with a red caret, into assets/logo.

    app-icon.svg, app-icon-dark.svg   macOS app icons, light and dark
    wordmark.svg, wordmark-dark.svg   the wordmark for light and dark backgrounds
    logo-mark.svg                     one-colour mark the app tints (empty editor)

Run script/render-icons afterwards for the PNGs. Same requirements as
generate.py.

    python3 assets/logo/explorations/final.py
"""

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


if __name__ == "__main__":
    main()
