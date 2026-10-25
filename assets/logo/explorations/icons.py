#!/usr/bin/env python3
"""Draws the app icon explorations for Me, one per wordmark palette.

Writes icons/NN-name.svg: a 1024 px macOS icon (an 824 px superellipse body
inset 100 px, with its drop shadow). Shares fonts and shaping with
generate.py, so it has the same requirements.

    python3 assets/logo/explorations/icons.py
"""

import math
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from generate import HERE, M_PIXELS, Run, f  # noqa: E402


def squircle(a=412.0, n=5.0, steps=192):
    """The macOS icon body: a superellipse centred on the 1024 canvas."""
    pts = []
    for i in range(steps):
        t = 2 * math.pi * i / steps
        c, s = math.cos(t), math.sin(t)
        pts.append((512 + a * math.copysign(abs(c) ** (2 / n), c),
                    512 + a * math.copysign(abs(s) ** (2 / n), s)))
    return "M" + "L".join(f"{f(x)} {f(y)}" for x, y in pts) + "Z"


class Icon:
    """`art` is drawn around (0, 0), the centre of the body, and clipped to it."""

    def __init__(self, slug, title, top, bottom, shadow, art, defs=""):
        self.slug, self.title = slug, title
        self.top, self.bottom, self.shadow = top, bottom, shadow
        self.art, self.defs = art, defs

    def svg(self):
        uid = self.slug
        body = squircle()
        return (
            '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024" width="1024" height="1024">'
            "<defs>"
            f'<linearGradient id="{uid}-bg" x1="0" y1="0" x2="0" y2="1">'
            f'<stop offset="0" stop-color="{self.top}"/><stop offset="1" stop-color="{self.bottom}"/></linearGradient>'
            f'<filter id="{uid}-shadow" x="-10%" y="-10%" width="120%" height="125%">'
            f'<feDropShadow dx="0" dy="14" stdDeviation="16" flood-color="{self.shadow}" flood-opacity="0.4"/></filter>'
            f'<clipPath id="{uid}-body"><path d="{body}"/></clipPath>'
            f"{self.defs.replace('{id}', uid)}"
            "</defs>"
            f'<path d="{body}" fill="url(#{uid}-bg)" filter="url(#{uid}-shadow)"/>'
            f'<g clip-path="url(#{uid}-body)"><g transform="translate(512 512)">{self.art.replace("{id}", uid)}</g></g>'
            "</svg>\n"
        )


def rrect(x, y, w, h, r, fill, extra=""):
    return f'<rect x="{f(x)}" y="{f(y)}" width="{f(w)}" height="{f(h)}" rx="{f(r)}" fill="{fill}"{extra}/>'


def centred(run, cx=0.0, cy=0.0):
    """A transform that centres a Run's ink box on (cx, cy)."""
    x0, y0, x1, y1 = run.box()
    return f"translate({f(cx - (x0 + x1) / 2)} {f(cy - (y0 + y1) / 2)})"


# ---------------------------------------------------------------------------
# 01 I-beam: the word "me" selected, the text pointer at its end.

def ibeam():
    ink, sel, beam = "#2B1A8C", "#C4B8FF", "#FF6A1F"
    r = Run("bricolage-xb", "me", 250, tracking=-0.02)
    x0, _, x1, _ = r.box()
    top, bottom = -r.xh - 62, 58
    sx0, sx1 = x0 - 34, x1 + 30
    bx = sx1 + 36  # the pointer's stem, centred on the selection's end
    art = (
        rrect(sx0, top, sx1 - sx0, bottom - top, 30, sel)
        + f'<path d="{r.d()}" fill="{ink}"/>'
        + rrect(bx - 22, top - 70, 44, bottom - top + 140, 22, beam)
        + rrect(bx - 76, top - 92, 152, 44, 22, beam)
        + rrect(bx - 76, bottom + 48, 152, 44, 22, beam)
    )
    w = bx + 76 - sx0
    tx, ty = -sx0 - w / 2, -(top + bottom) / 2
    return Icon("01-ibeam", "I-beam", "#3D29B8", "#2B1A8C", "#170C55",
                f'<g transform="translate({f(tx)} {f(ty)})">{art}</g>')


# 02 Nib: a fountain pen nib that has just written a full stop.

def nib():
    light, dark, hole, collar, gap = "#A1284D", "#7A1636", "#F2A516", "#5C0F27", "#FBD9CF"
    outline = "M-150 -260 Q0 -285 150 -260 L162 -70 C162 50 70 160 0 310 C-70 160 -162 50 -162 -70 Z"
    defs = (f'<clipPath id="{{id}}-nib"><path d="{outline}"/></clipPath>')
    nib_art = (
        f'<g clip-path="url(#{{id}}-nib)">'
        + rrect(-200, -300, 200, 620, 0, light)
        + rrect(0, -300, 200, 620, 0, dark)
        + rrect(-200, -300, 400, 105, 0, collar)
        + rrect(-7, 0, 14, 330, 0, gap)
        + "</g>"
        + f'<circle cx="0" cy="-10" r="38" fill="{gap}"/>'
        + f'<circle cx="0" cy="-10" r="24" fill="{hole}"/>'
    )
    art = (f'<g transform="translate(40 -50) rotate(32)">{nib_art}</g>'
           f'<circle cx="-170" cy="235" r="46" fill="{hole}"/>')
    return Icon("02-nib", "Nib", "#FDE6DE", "#F5BFAE", "#9E3B4F", art, defs)


# 03 Cursor: a terminal block cursor sitting on the m, which it inverts.

def cursor():
    pine, mint, block = "#0F3D38", "#86F0C6", "#FFC94A"
    r = Run("martian-mono", "me", 300)
    cell = r.glyphs[0]["adv"]
    top, bottom = -r.cap - 36, 64
    w = 2 * cell
    art = (
        f'<g transform="translate({f(-w / 2)} {f((r.cap - 28) / 2)})">'
        + rrect(-8, top, cell + 16, bottom - top, 22, block)
        + f'<path d="{r.d(0)}" fill="{pine}"/><path d="{r.d(1)}" fill="{mint}"/>'
        + "</g>"
    )
    return Icon("03-cursor", "Cursor", "#155049", "#0D3531", "#03201C", art)


# 04 Stripes: the Groove m on its own.

def stripes():
    colors = ["#FF5C8A", "#FF9A3D", "#FFD447"]
    h, r = 176.0, 52.0
    d = (f"M0 0 V{f(-h + r)} A{f(r)} {f(r)} 0 0 1 {f(2 * r)} {f(-h + r)} V0 "
         f"M{f(2 * r)} {f(-h + r)} A{f(r)} {f(r)} 0 0 1 {f(4 * r)} {f(-h + r)} V0")
    widths = [78, 58, 46, 26, 14]
    stroke = 'fill="none" stroke-linecap="butt" stroke-linejoin="round"'
    mask = "".join(f'<path d="{d}" {stroke} stroke="{c}" stroke-width="{w}"/>'
                   for w, c in zip(widths, ["#fff", "#000", "#fff", "#000", "#fff"]))
    paint = "".join(f'<path d="{d}" {stroke} stroke="{c}" stroke-width="{w}"/>'
                    for w, c in zip(widths[::2], colors))
    defs = (f'<mask id="{{id}}-stripes" maskUnits="userSpaceOnUse" x="-100" y="-300" width="500" height="400">'
            f"{mask}</mask>")
    # The m's centre is (2r, -h/2); at 2.3x it fills about 70% of the body.
    art = (f'<g transform="scale(2.3) translate({f(-2 * r)} {f(h / 2)})">'
           f'<g mask="url(#{{id}}-stripes)">{paint}</g></g>')
    return Icon("04-stripes", "Stripes", "#3A2690", "#24175E", "#100834", art, defs)


# 05 Keycap: a key with the signature on it.

def keycap():
    base, face0, face1, ink = "#F0AE86", "#FFEEDD", "#FFDCC0", "#D41A6A"
    r = Run("mr-dafoe", "Me", 250)
    defs = (f'<linearGradient id="{{id}}-face" x1="0" y1="0" x2="0" y2="1">'
            f'<stop offset="0" stop-color="{face0}"/><stop offset="1" stop-color="{face1}"/></linearGradient>'
            f'<filter id="{{id}}-key" x="-20%" y="-20%" width="140%" height="150%">'
            f'<feDropShadow dx="0" dy="26" stdDeviation="22" flood-color="#8A0C40" flood-opacity="0.45"/></filter>')
    art = (
        rrect(-280, -250, 560, 560, 112, base, ' filter="url(#{id}-key)"')
        + rrect(-228, -238, 456, 456, 84, "url(#{id}-face)")
        + f'<path d="{r.d()}" fill="{ink}" transform="{centred(r, 0, -16)}"/>'
    )
    return Icon("05-keycap", "Keycap", "#E8306F", "#FF7A2E", "#7A0A38", art, defs)


# 06 Overprint: two arches printed in two inks; the shared stem is where they mix.

def overprint():
    pink, blue, mix = "#FF5CAA", "#3D7BFF", "#3D2CAA"
    arch = "M-300 200 V-40 A180 180 0 0 1 60 -40 V200 H-60 V-40 A60 60 0 0 0 -180 -40 V200 Z"
    shift = 252
    defs = f'<clipPath id="{{id}}-right"><path d="{arch}" transform="translate({shift} 0)"/></clipPath>'
    art = (
        f'<g transform="translate({f(-shift / 2 + 120)} 10)">'
        f'<path d="{arch}" fill="{pink}"/>'
        f'<path d="{arch}" fill="{blue}" transform="translate({shift} 0)"/>'
        f'<path d="{arch}" fill="{mix}" clip-path="url(#{{id}}-right)"/>'
        "</g>"
    )
    return Icon("06-overprint", "Overprint", "#FFF6D1", "#FFE88F", "#B38A1F", art, defs)


# 07 Pencil: a pixel pencil drawing a pixel line, after the paint tools.

def pencil():
    outline, pink, light, dark, wood = "#5A23E0", "#FF4FA0", "#FF9CCB", "#D92E80", "#FFC977"
    # A 16 x 16 sprite. The pencil lies on the diagonal x + y = 15; p = x + y
    # runs across it and a = y - x along it, from the eraser (top right) to
    # the point (bottom left).
    cells = {}
    for y in range(16):
        for x in range(16):
            p, a = x + y, y - x
            if -14 <= a <= -10 and 14 <= p <= 16:
                cells[x, y] = light  # eraser
            elif -9 <= a <= -8 and 14 <= p <= 16:
                cells[x, y] = outline  # ferrule
            elif -7 <= a <= 4 and 14 <= p <= 16:
                cells[x, y] = {14: light, 15: pink, 16: dark}[p]
            elif 5 <= a <= 7 and 14 <= p <= 16 or a == 8 and p == 15:
                cells[x, y] = wood
            elif 9 <= a <= 10 and 14 <= p <= 16 or a == 11 and p == 15:
                cells[x, y] = outline  # lead
            elif -14 <= a <= 6 and p in (13, 17) or a == -15 and 13 <= p <= 17:
                cells[x, y] = outline
            elif 7 <= a <= 8 and p in (13, 17) or a == 9 and p in (13, 17):
                cells[x, y] = outline
    # The line it has drawn, from the point to the right.
    for x, y in ((3, 14), (4, 14), (5, 15), (6, 15), (7, 15), (8, 14), (9, 14), (10, 14), (11, 15), (12, 15), (13, 15)):
        cells[x, y] = outline
    px = 36
    art = "".join(rrect((x - 8) * px, (y - 8) * px, px, px, 0, c) for (x, y), c in cells.items())
    return Icon("07-pencil", "Pencil", "#DDF86E", "#BFEA3A", "#5B7A06", f'<g transform="translate(0 -14)">{art}</g>')


# 08 Paragraph sun: the bottom of the sun is lines of ragged text.

def paragraph_sun():
    c0, c1, c2 = "#FFD166", "#FF8A4C", "#FF4E6A"
    R, cy = 270, 10
    defs = (f'<linearGradient id="{{id}}-sun" gradientUnits="userSpaceOnUse" x1="0" y1="{cy - R}" x2="0" y2="{cy + R}">'
            f'<stop offset="0" stop-color="{c0}"/><stop offset="0.55" stop-color="{c1}"/>'
            f'<stop offset="1" stop-color="{c2}"/></linearGradient>'
            f'<clipPath id="{{id}}-disc"><circle cx="0" cy="{cy}" r="{R}"/></clipPath>')
    bars = [rrect(-R, cy - R, 2 * R, R + 4, 0, "url(#{id}-sun)")]
    y, ends = cy + 32, [250, 120, 200, 40]
    for i, (h, end) in enumerate(zip([44, 38, 32, 26], ends)):
        bars.append(rrect(-R - 40, y, R + 40 + end, h, h / 2, "url(#{id}-sun)"))
        y += h + 22 + i * 4
    art = f'<g clip-path="url(#{{id}}-disc)">{"".join(bars)}</g>'
    return Icon("08-paragraph-sun", "Paragraph sun", "#43197A", "#2A0F4A", "#12041F",
                f'<g transform="translate(0 -6)">{art}</g>', defs)


# 09 Ribbon: an M folded from one strip, front cobalt, back vermilion.

def ribbon():
    front, back, front_fold, back_fold = "#2F45FF", "#FF4D3A", "#2234D6", "#E03A28"
    w, top, bot, v = 124, -230, 230, 120
    left = f"M-270 {bot} V{top} H{-270 + w} V{bot} Z"
    right = f"M{270 - w} {bot} V{top} H270 V{bot} Z"
    diag_l = f"M-270 {top} H{-270 + w} L{w / 2} {v} H{-w / 2} Z"
    diag_r = f"M{270 - w} {top} H270 L{w / 2} {v} H{-w / 2} Z"
    defs = (f'<clipPath id="{{id}}-l"><path d="{left}"/></clipPath>'
            f'<clipPath id="{{id}}-r"><path d="{diag_r}"/></clipPath>')
    art = (
        f'<path d="{left}" fill="{front}"/>'
        f'<path d="{diag_l}" fill="{back}"/>'
        f'<path d="{diag_l}" fill="{back_fold}" clip-path="url(#{{id}}-l)"/>'
        f'<path d="{diag_r}" fill="{front}"/>'
        f'<path d="{diag_l}" fill="{front_fold}" clip-path="url(#{{id}}-r)"/>'
        f'<path d="{right}" fill="{back}"/>'
        f'<path d="{diag_r}" fill="{back_fold}" clip-path="url(#{{id}}-rr)"/>'
    )
    defs += f'<clipPath id="{{id}}-rr"><path d="{right}"/></clipPath>'
    return Icon("09-ribbon", "Ribbon", "#FFEAE3", "#FFD3C5", "#B0563E", art, defs)


# 10 Retro M: the Retro wordmark's M, extruded in two bands.

def retro_m():
    face, band1, band2 = "#FFD76A", "#FF8E3C", "#E2433A"
    r = Run("caprasimo", "M", 470)
    defs = f'<path id="{{id}}-m" d="{r.d()}"/>'
    steps = "".join(
        f'<use href="#{{id}}-m" x="{i}" y="{i}" fill="{band2 if i > 18 else band1}"/>' for i in range(36, 0, -2)
    )
    art = f'<g transform="{centred(r, -18, -18)}">{steps}<use href="#{{id}}-m" fill="{face}"/></g>'
    return Icon("10-retro-m", "Retro M", "#106679", "#0A4552", "#022A33", art, defs)


ICONS = [ibeam, nib, cursor, stripes, keycap, overprint, pencil, paragraph_sun, ribbon, retro_m]


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else os.path.join(HERE, "icons")
    os.makedirs(out, exist_ok=True)
    for make in ICONS:
        icon = make()
        with open(os.path.join(out, icon.slug + ".svg"), "w") as fh:
            fh.write(icon.svg())
        print(icon.slug)


if __name__ == "__main__":
    main()
