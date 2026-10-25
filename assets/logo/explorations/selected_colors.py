#!/usr/bin/env python3
"""Draws the Selected wordmark and its app icon in ten colour schemes.

Writes selected/NN-name.svg (the wordmark) and selected/NN-name-icon.svg (the
wordmark on a macOS icon body in the same background colour). Same
requirements as generate.py.

    python3 assets/logo/explorations/selected_colors.py
"""

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from generate import HERE, selected  # noqa: E402
from icons import Icon  # noqa: E402

# slug, name, background, selection, ink, caret.
PALETTES = [
    ("01-highlighter", "Highlighter", "#FFF7D6", "#FFE14D", "#14452F", "#FF4A2E"),
    ("02-ocean", "Ocean", "#E4F3FF", "#A6D8FF", "#0B2E59", "#FF6B4A"),
    ("03-terracotta", "Terracotta", "#FCEADC", "#FFC4A0", "#7A2E14", "#009C93"),
    ("04-cherry", "Cherry", "#FFE6EA", "#FFB8C4", "#A8102C", "#FFAA00"),
    ("05-citrus", "Citrus", "#F3FAD6", "#D4F25A", "#34420A", "#FF7300"),
    ("06-tangerine", "Tangerine", "#FF7A2E", "#FFD98A", "#4A1806", "#1557FF"),
    ("07-electric", "Electric", "#1D5FE0", "#0B348C", "#FFE45C", "#FF6F91"),
    ("08-aqua", "Aqua", "#3CCFC0", "#B3F5EC", "#06343F", "#FF6A1A"),
    ("09-night-shift", "Night shift", "#07343C", "#156B74", "#A6F4DE", "#FFB020"),
    ("10-espresso", "Espresso", "#2A1912", "#6A3A20", "#FFCDA6", "#34D6C4"),
    # Round 3: complementary highlights, an inverted one, and a neon one.
    ("11-mustard", "Mustard", "#F2B33D", "#FFE9A6", "#12305A", "#E63946"),
    ("12-sage-clay", "Sage and clay", "#D7E4D0", "#F4B9A3", "#1E3A2B", "#E8603C"),
    ("13-tomato-mint", "Tomato and mint", "#EC4B2F", "#A9F0D1", "#3A0C05", "#FFD23F"),
    ("14-petrol", "Petrol and sunflower", "#0B6A85", "#FFD166", "#082B36", "#FF5A5F"),
    ("15-bubblegum", "Bubblegum", "#FF94BC", "#FFF2A8", "#173C8C", "#00A884"),
    ("16-moss", "Moss", "#2C4A1C", "#5E7F24", "#EAF7A6", "#FF8C42"),
    ("17-rose-forest", "Rose and forest", "#F6CCC6", "#A3DBB8", "#1F4D3A", "#E2533A"),
    ("18-sky-sun", "Sky and sun", "#9ACFFF", "#FFE66D", "#0B2545", "#FF5D3A"),
    ("19-focused", "Focused", "#DCE8F7", "#183F8C", "#FFE9A8", "#FF7A45"),
    ("20-neon", "Neon", "#0D3B2E", "#C8FF3D", "#0D3B2E", "#FF4FA0"),
    # Round 4: saturated, no pastels; the highlight is a strong second colour.
    ("21-racing-lemon", "Racing green and lemon", "#0E5A3A", "#FFE11A", "#0A3D27", "#FF3B1F"),
    ("22-cobalt-orange", "Cobalt and orange", "#1240C9", "#FF7A00", "#0A1F66", "#FFE11A"),
    ("23-tomato-navy", "Tomato and navy", "#E8361E", "#0D2B6B", "#FFC93C", "#2BD9C4"),
    ("24-emerald-pink", "Emerald and hot pink", "#00936B", "#FF4F8B", "#003B2A", "#FFD000"),
    ("25-ketchup-mustard", "Ketchup and mustard", "#FFC400", "#D7261E", "#FFE07A", "#0B3EBF"),
    ("26-teal-coral", "Teal and coral", "#007C80", "#FF6B4A", "#052E30", "#FFD23F"),
    ("27-rust-turquoise", "Rust and turquoise", "#B5401A", "#19C2B0", "#2B0B02", "#FFC52E"),
    ("28-blue-lime", "Blue and acid lime", "#0050C8", "#B8F000", "#072A66", "#FF4F8B"),
    ("29-crimson-teal", "Crimson and teal", "#B0102F", "#12B5A6", "#2A0610", "#FFC93C"),
    ("30-pink-yellow", "Hot pink and yellow", "#F0287A", "#FFD400", "#6B0026", "#00A86B"),
    # Round 5: normal colours, the way selected text really looks in macOS,
    # Windows and well-known editor themes.
    ("31-mac", "Mac", "#FFFFFF", "#B3D7FF", "#1D1D1F", "#007AFF"),
    ("32-mac-dark", "Mac dark", "#1E1E1E", "#3F638B", "#FFFFFF", "#0A84FF"),
    ("33-highlighter-paper", "Highlighter on paper", "#FFFFFF", "#FFEB3B", "#212121", "#E53935"),
    ("34-windows", "Windows", "#FFFFFF", "#0078D7", "#FFFFFF", "#1D1D1F"),
    ("35-paper", "Paper, orange caret", "#FAF7F0", "#B3D7FF", "#1D1D1F", "#FF6A1F"),
    ("36-blue", "Blue", "#0A6CFF", "#FFFFFF", "#0A6CFF", "#FF8A00"),
    ("37-monokai", "Monokai", "#272822", "#49483E", "#A6E22E", "#F92672"),
    ("38-solarized-dark", "Solarized dark", "#002B36", "#268BD2", "#FDF6E3", "#CB4B16"),
    ("39-solarized-light", "Solarized light", "#FDF6E3", "#2AA198", "#FDF6E3", "#DC322F"),
    ("40-github-dark", "GitHub dark", "#0D1117", "#1F6FEB", "#E6EDF3", "#F78166"),
]


def mix(hex_color, toward, amount):
    a = [int(hex_color[i:i + 2], 16) for i in (1, 3, 5)]
    b = [int(toward[i:i + 2], 16) for i in (1, 3, 5)]
    return "#" + "".join(f"{round(x + (y - x) * amount):02X}" for x, y in zip(a, b))


def icon(mark):
    """The wordmark centred on an icon body of its own background colour."""
    x0, y0, x1, y1 = mark.box
    w, h = x1 - x0, y1 - y0
    scale = min(824 * 0.66 / w, 824 * 0.5 / h)
    art = (f'<g transform="scale({scale:.4f}) translate({-(x0 + x1) / 2:.2f} {-(y0 + y1) / 2:.2f})">'
           f"{mark.markup(mark.slug + '-icon')}</g>")
    # A little lighter at the top; the shadow is the background, much darker.
    return Icon(mark.slug + "-icon", mark.title, mix(mark.bg, "#FFFFFF", 0.1), mark.bg,
                mix(mark.bg, "#000000", 0.6), art)


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else os.path.join(HERE, "selected")
    os.makedirs(out, exist_ok=True)
    for slug, name, bg, sel, ink, caret in PALETTES:
        mark = selected(ink, sel, caret, bg, slug, name, ("Ink", "Selection", "Caret", "Background"))
        with open(os.path.join(out, slug + ".svg"), "w") as fh:
            fh.write(mark.wordmark_svg())
        with open(os.path.join(out, slug + "-icon.svg"), "w") as fh:
            fh.write(icon(mark).svg())
        print(slug)


if __name__ == "__main__":
    main()
