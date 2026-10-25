#!/usr/bin/env python3
"""Draws the wordmark explorations for Me as outlined SVGs.

Every letter is converted to paths, so the SVGs look the same everywhere and
need no fonts installed. For each direction it writes:

    NN-name.svg       the wordmark on a transparent background
    NN-name-icon.svg  the wordmark in a 1024 px macOS app icon

Needs fonttools and uharfbuzz (pip install fonttools uharfbuzz). The fonts
are all from Google Fonts (SIL Open Font License); they are downloaded once
into $ME_LOGO_FONTS, or ~/.cache/me-logo-fonts.

    python3 assets/logo/explorations/generate.py
"""

import math
import os
import re
import sys
import urllib.request

import uharfbuzz as hb
from fontTools.pens.boundsPen import BoundsPen
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen
from fontTools.ttLib import TTFont

HERE = os.path.dirname(os.path.abspath(__file__))
FONT_DIR = os.environ.get("ME_LOGO_FONTS", os.path.expanduser("~/.cache/me-logo-fonts"))

# Google Fonts css2 family specs, one static instance each.
FONTS = {
    "bricolage-xb": "Bricolage+Grotesque:opsz,wght@96,800",
    "instrument-italic": "Instrument+Serif:ital@1",
    "martian-mono": "Martian+Mono:wdth,wght@112.5,700",
    "mr-dafoe": "Mr+Dafoe",
    "fraunces-black": "Fraunces:opsz,wght,SOFT,WONK@144,900,100,1",
    "unbounded-black": "Unbounded:wght@900",
    "rubik-black": "Rubik:wght@900",
    "caprasimo": "Caprasimo",
}


def font_path(key):
    path = os.path.join(FONT_DIR, key + ".ttf")
    if not os.path.exists(path):
        os.makedirs(FONT_DIR, exist_ok=True)
        css_url = f"https://fonts.googleapis.com/css2?family={FONTS[key]}"
        # An old user agent makes the css API answer with TrueType files.
        req = urllib.request.Request(css_url, headers={"User-Agent": "curl/8"})
        css = urllib.request.urlopen(req).read().decode()
        ttf_url = re.search(r"url\((https://fonts\.gstatic\.com[^)]+)\)", css).group(1)
        with open(path, "wb") as f:
            f.write(urllib.request.urlopen(ttf_url).read())
    return path


_fonts = {}


def _font(key):
    if key not in _fonts:
        path = font_path(key)
        face = hb.Face(hb.Blob.from_file_path(path))
        _fonts[key] = (TTFont(path), hb.Font(face), face.upem)
    return _fonts[key]


class Run:
    """A shaped line of text: one path per glyph, baseline at y = 0, y down."""

    def __init__(self, font, text, size, x=0.0, tracking=0.0):
        tt, hbfont, upem = _font(font)
        buf = hb.Buffer()
        buf.add_str(text)
        buf.guess_segment_properties()
        hb.shape(hbfont, buf, {"kern": True, "liga": True})
        glyphset = tt.getGlyphSet()
        order = tt.getGlyphOrder()
        s = size / upem
        self.glyphs = []
        for info, pos in zip(buf.glyph_infos, buf.glyph_positions):
            glyph = glyphset[order[info.codepoint]]
            gx, gy = x + pos.x_offset * s, -pos.y_offset * s
            pen = SVGPathPen(glyphset, ntos=lambda v: f"{v:.2f}".rstrip("0").rstrip("."))
            glyph.draw(TransformPen(pen, (s, 0, 0, -s, gx, gy)))
            bp = BoundsPen(glyphset)
            glyph.draw(bp)
            b = bp.bounds
            box = (gx + b[0] * s, gy - b[3] * s, gx + b[2] * s, gy - b[1] * s) if b else None
            self.glyphs.append({"d": pen.getCommands(), "box": box, "x": gx, "adv": pos.x_advance * s})
            x += pos.x_advance * s + tracking * size
        self.end = x
        os2 = tt["OS/2"]
        self.cap = getattr(os2, "sCapHeight", 0) * s
        self.xh = getattr(os2, "sxHeight", 0) * s

    def d(self, i=None):
        if i is None:
            return " ".join(g["d"] for g in self.glyphs)
        return self.glyphs[i]["d"]

    def box(self, i=None):
        bs = [g["box"] for g in (self.glyphs if i is None else [self.glyphs[i]]) if g["box"]]
        return (min(b[0] for b in bs), min(b[1] for b in bs), max(b[2] for b in bs), max(b[3] for b in bs))


def union(*boxes):
    return (min(b[0] for b in boxes), min(b[1] for b in boxes), max(b[2] for b in boxes), max(b[3] for b in boxes))


def f(v):
    return f"{v:.2f}".rstrip("0").rstrip(".")


class Design:
    """`body` is SVG markup in wordmark coordinates; ids in it are written as
    `{id}-name` so several copies can share a page."""

    def __init__(self, slug, title, bg, swatches, body, box, defs="", pad=0.08):
        self.slug, self.title, self.bg, self.swatches = slug, title, bg, swatches
        self.body, self.defs = body, defs
        w, h = box[2] - box[0], box[3] - box[1]
        p = pad * max(w, h)
        self.box = (box[0] - p, box[1] - p, box[2] + p, box[3] + p)

    def markup(self, uid):
        defs = f"<defs>{self.defs}</defs>" if self.defs else ""
        return (defs + self.body).replace("{id}", uid)

    def wordmark_svg(self):
        x0, y0, x1, y1 = self.box
        w, h = x1 - x0, y1 - y0
        return (
            f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="{f(x0)} {f(y0)} {f(w)} {f(h)}" '
            f'width="{f(w)}" height="{f(h)}">{self.markup(self.slug)}</svg>\n'
        )

    def icon_svg(self):
        # macOS grid: an 824 pt body inset 100 pt on a 1024 pt canvas. The
        # wordmark fills 62% of the body's width, or 46% of its height.
        x0, y0, x1, y1 = self.box
        w, h = x1 - x0, y1 - y0
        scale = min(824 * 0.66 / w, 824 * 0.5 / h)
        cx, cy = (x0 + x1) / 2, (y0 + y1) / 2
        t = f"translate(512 512) scale({f(scale)}) translate({f(-cx)} {f(-cy)})"
        return (
            '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024" width="1024" height="1024">'
            f'<rect x="100" y="100" width="824" height="824" rx="186" fill="{self.bg}"/>'
            f'<g transform="{t}">{self.markup(self.slug + "-icon")}</g></svg>\n'
        )


# ---------------------------------------------------------------------------
# 01 Selected: the word selected in the editor, caret at the end.

def selected(ink="#2B1A8C", sel="#C4B8FF", caret="#FF6A1F", bg="#EFEAFF", slug="01-selected", title="Selected",
             names=("Indigo", "Selection", "Caret", "Lilac")):
    r = Run("bricolage-xb", "me", 200, tracking=-0.02)
    x0, _, x1, _ = r.box()
    top, bottom = -r.xh - 50, 46
    caret_x = x1 + 22
    body = (
        f'<rect x="{f(x0 - 26)}" y="{f(top)}" width="{f(caret_x - x0 + 26)}" height="{f(bottom - top)}" rx="18" fill="{sel}"/>'
        f'<path d="{r.d()}" fill="{ink}"/>'
        f'<rect x="{f(caret_x)}" y="{f(top - 14)}" width="17" height="{f(bottom - top + 28)}" rx="8.5" fill="{caret}"/>'
    )
    box = (x0 - 26, top - 14, caret_x + 17, bottom + 14)
    return Design(slug, title, bg, list(zip(names, (ink, sel, caret, bg))), body, box)


# 02 Ink: a literary italic with a full stop, for the reading half of Me.

def ink():
    ink, dot, bg = "#7A1636", "#F2A516", "#FBD9CF"
    r = Run("instrument-italic", "Me.", 260, tracking=-0.01)
    body = f'<path d="{r.d(0)} {r.d(1)}" fill="{ink}"/><path d="{r.d(2)}" fill="{dot}"/>'
    return Design("02-ink", "Ink", bg, [("Claret", ink), ("Saffron", dot), ("Blush", bg)], body, r.box())


# 03 Prompt: the `me` command at a prompt, block cursor waiting.

def prompt():
    ink, cursor, bg = "#86F0C6", "#FFC94A", "#0F3D38"
    r = Run("martian-mono", "me", 200)
    cell = r.glyphs[1]["adv"]
    x, w = r.end + 14, cell * 0.62
    top = -r.xh
    body = (
        f'<path d="{r.d()}" fill="{ink}"/>'
        f'<rect x="{f(x)}" y="{f(top)}" width="{f(w)}" height="{f(r.xh)}" rx="6" fill="{cursor}"/>'
    )
    box = union(r.box(), (x, top, x + w, 0))
    return Design("03-prompt", "Prompt", bg, [("Mint", ink), ("Cursor", cursor), ("Pine", bg)], body, box)


# 04 Groove: a drawn monoline "me" in three parallel stripes.

def groove():
    colors, bg = ["#FF5C8A", "#FF9A3D", "#FFD447"], "#2A1B6E"
    h, r = 176.0, 52.0  # x-height and arch radius of the centreline
    e_r = h / 2
    m = (f"M0 0 V{f(-h + r)} A{f(r)} {f(r)} 0 0 1 {f(2 * r)} {f(-h + r)} V0 "
         f"M{f(2 * r)} {f(-h + r)} A{f(r)} {f(r)} 0 0 1 {f(4 * r)} {f(-h + r)} V0")
    ex = 4 * r + 104 + e_r
    a = math.radians(38)
    e = (f"M{f(ex - e_r)} {f(-e_r)} H{f(ex + e_r)} "
         f"A{f(e_r)} {f(e_r)} 0 1 0 {f(ex + e_r * math.cos(a))} {f(-e_r + e_r * math.sin(a))}")
    d = m + " " + e
    # Stripes are painted from the outside in; the gaps are cut with a mask
    # so the wordmark stays transparent.
    widths = [78, 58, 46, 26, 14]
    stroke = 'fill="none" stroke-linecap="butt" stroke-linejoin="round"'
    mask = "".join(
        f'<path d="{d}" {stroke} stroke="{c}" stroke-width="{w}"/>'
        for w, c in zip(widths, ["#fff", "#000", "#fff", "#000", "#fff"])
    )
    paint = "".join(
        f'<path d="{d}" {stroke} stroke="{c}" stroke-width="{w}"/>'
        for w, c in zip(widths[::2], colors)
    )
    defs = (f'<mask id="{{id}}-stripes" maskUnits="userSpaceOnUse" x="-100" y="-300" width="800" height="500">'
            f"{mask}</mask>")
    body = f'<g mask="url(#{{id}}-stripes)">{paint}</g>'
    hw = widths[0] / 2
    box = (-hw, -h - hw, ex + e_r + hw, hw)
    return Design("04-groove", "Groove", bg,
                  [("Pink", colors[0]), ("Tangerine", colors[1]), ("Marigold", colors[2]), ("Ultramarine", bg)],
                  body, box, defs)


# 05 Signature: signed, like a name on your own work.

def signature():
    c0, c1, bg = "#D41A6A", "#FF7A2E", "#FFE4CC"
    r = Run("mr-dafoe", "Me", 300)
    x0, y0, x1, y1 = r.box()
    # A tapered swash under the word: thick in the middle, pointed ends.
    sx0, sx1, sy = x0 + 90, x1 + 20, 40
    swash = (f"M{f(sx0)} {f(sy + 10)} "
             f"Q{f((sx0 + sx1) / 2)} {f(sy - 26)} {f(sx1)} {f(sy - 30)} "
             f"Q{f((sx0 + sx1) / 2)} {f(sy + 14)} {f(sx0)} {f(sy + 10)} Z")
    defs = (f'<linearGradient id="{{id}}-ink" gradientUnits="userSpaceOnUse" x1="{f(x0)}" y1="0" x2="{f(sx1)}" y2="0">'
            f'<stop offset="0" stop-color="{c0}"/><stop offset="1" stop-color="{c1}"/></linearGradient>')
    body = f'<path d="{r.d()} {swash}" fill="url(#{{id}}-ink)"/>'
    box = union(r.box(), (sx0, sy - 34, sx1, sy + 16))
    return Design("05-signature", "Signature", bg, [("Magenta", c0), ("Orange", c1), ("Peach", bg)], body, box, defs)


# 06 Riso: two inks printed over each other; where they overlap they mix.

def riso():
    pink, blue, mix, bg = "#FF5CAA", "#3D7BFF", "#3D2CAA", "#FFF0B5"
    r = Run("fraunces-black", "Me", 240, tracking=-0.17)
    m, e = r.d(0), r.d(1)
    defs = f'<clipPath id="{{id}}-e"><path d="{e}"/></clipPath>'
    body = (
        f'<path d="{m}" fill="{pink}"/><path d="{e}" fill="{blue}"/>'
        f'<path d="{m}" fill="{mix}" clip-path="url(#{{id}}-e)"/>'
    )
    return Design("06-riso", "Riso", bg,
                  [("Fluoro pink", pink), ("Riso blue", blue), ("Overprint", mix), ("Butter", bg)], body, r.box(), defs)


# 07 Pixel: a bitmap "me" with a drop shadow, after the paint tools.

M_PIXELS = [
    "XXXXX.XXXX.",
    "XXXXXXXXXXX",
    "XX...XX..XX",
    "XX...XX..XX",
    "XX...XX..XX",
    "XX...XX..XX",
    "XX...XX..XX",
]
E_PIXELS = [
    "..XXXXX.",
    ".XXXXXXX",
    "XX....XX",
    "XXXXXXXX",
    "XX......",
    ".XXXXXXX",
    "..XXXXXX",
]


def pixel():
    ink, shadow, bg = "#5A23E0", "#FF4FA0", "#D5F55B"
    px = 24
    rects = []
    for ox, rows in ((0, M_PIXELS), (len(M_PIXELS[0]) + 2, E_PIXELS)):
        for y, row in enumerate(rows):
            for x, c in enumerate(row):
                if c == "X":
                    rects.append(((ox + x) * px, (y - len(rows)) * px))

    def layer(dx, color):
        d = "".join(f"M{x + dx} {y + dx}h{px}v{px}h-{px}z" for x, y in rects)
        return f'<path d="{d}" fill="{color}"/>'

    w = (len(M_PIXELS[0]) + 2 + len(E_PIXELS[0])) * px
    body = layer(px // 2, shadow) + layer(0, ink)
    return Design("07-pixel", "Pixel", bg, [("Violet", ink), ("Hot pink", shadow), ("Lime", bg)],
                  body, (0, -7 * px, w + px // 2, px // 2))


# 08 Sunset: very heavy, letters fused, a warm gradient on plum.

def sunset():
    c0, c1, c2, bg = "#FF4E6A", "#FF8A4C", "#FFD166", "#2E1250"
    r = Run("unbounded-black", "me", 220, tracking=-0.06)
    x0, y0, x1, y1 = r.box()
    defs = (f'<linearGradient id="{{id}}-sun" gradientUnits="userSpaceOnUse" x1="{f(x0)}" y1="{f(y1)}" x2="{f(x1)}" y2="{f(y0)}">'
            f'<stop offset="0" stop-color="{c0}"/><stop offset="0.55" stop-color="{c1}"/>'
            f'<stop offset="1" stop-color="{c2}"/></linearGradient>')
    body = f'<path d="{r.d()}" fill="url(#{{id}}-sun)"/>'
    return Design("08-sunset", "Sunset", bg, [("Rose", c0), ("Apricot", c1), ("Sun", c2), ("Plum", bg)],
                  body, r.box(), defs)


# 09 Split: sliced through the x-height, the halves shifted apart.

def split():
    top_c, bot_c, bg = "#2F45FF", "#FF4D3A", "#FFE2D9"
    r = Run("rubik-black", "Me", 230, tracking=-0.02)
    x0, y0, x1, y1 = r.box()
    cut = -r.xh * 0.52
    shift = 14
    defs = (
        f'<clipPath id="{{id}}-top"><rect x="{f(x0 - 40)}" y="{f(y0 - 40)}" width="{f(x1 - x0 + 80)}" height="{f(cut - y0 + 40 - 5)}"/></clipPath>'
        f'<clipPath id="{{id}}-bot"><rect x="{f(x0 - 40)}" y="{f(cut + 5)}" width="{f(x1 - x0 + 80)}" height="{f(y1 - cut + 40)}"/></clipPath>'
    )
    body = (
        f'<path d="{r.d()}" fill="{top_c}" clip-path="url(#{{id}}-top)" transform="translate({-shift} 0)"/>'
        f'<path d="{r.d()}" fill="{bot_c}" clip-path="url(#{{id}}-bot)" transform="translate({shift} 0)"/>'
    )
    return Design("09-split", "Split", bg, [("Cobalt", top_c), ("Vermilion", bot_c), ("Shell", bg)],
                  body, (x0 - shift, y0, x1 + shift, y1), defs)


# 10 Retro: a soft 70s display face with a two-band extrusion.

def retro():
    face, band1, band2, bg = "#FFD76A", "#FF8E3C", "#E2433A", "#0C5161"
    r = Run("caprasimo", "Me", 230, tracking=0.0)
    d = r.d()
    defs = f'<path id="{{id}}-word" d="{d}"/>'
    steps = []
    for i in range(24, 0, -1):
        color = band2 if i > 12 else band1
        steps.append(f'<use href="#{{id}}-word" x="{i}" y="{i}" fill="{color}"/>')
    body = "".join(steps) + f'<use href="#{{id}}-word" fill="{face}"/>'
    x0, y0, x1, y1 = r.box()
    return Design("10-retro", "Retro", bg, [("Butter", face), ("Orange", band1), ("Brick", band2), ("Deep teal", bg)],
                  body, (x0, y0, x1 + 24, y1 + 24), defs)


DESIGNS = [selected, ink, prompt, groove, signature, riso, pixel, sunset, split, retro]


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else HERE
    for make in DESIGNS:
        design = make()
        with open(os.path.join(out, design.slug + ".svg"), "w") as fh:
            fh.write(design.wordmark_svg())
        with open(os.path.join(out, design.slug + "-icon.svg"), "w") as fh:
            fh.write(design.icon_svg())
        print(design.slug)


if __name__ == "__main__":
    main()
