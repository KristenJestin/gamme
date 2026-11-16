#!/usr/bin/env python3
"""Writes Gamme's mark and its icon from one pixel grid (issue #8, README "Brand").

    python3 tools/brand.py

The mark is the 8-bit eighth note, "Pixel note", on a grid of 7 x 8 cells. Everything below is
cut from it, so the SVGs and the PNGs cannot drift apart:

- assets/brand/mark.svg          the mark alone, in one colour (currentColor);
- assets/brand/mark-accent.svg   the mark alone, its head in the accent;
- packaging/linux/icons/hicolor/scalable/apps/gamme.svg   the icon, on a 32 x 32 grid;
- packaging/linux/icons/hicolor/<n>x<n>/apps/gamme.png    the icon at each size, each pixel of its
  grid a whole block of pixels: no resampling, no smoothing, only hard edges.

Three grids hold the icon, because a pixel of the grid must come out as a whole number of pixels
at every size: 16 (the mark's cell is 1 pixel) for 16, 32 (3 pixels) for 32 and 64 to 512, and 24
(2 pixels) for 48. 16 and 48 are drawn on their own grid, not scaled from the 32 one.

Standard library only.
"""

import struct
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# The palette, "Nocturne".
ACCENT = (0x7C, 0x5C, 0xFF)
PAPER = (0xF6, 0xF5, 0xFB)
NIGHT = (0x13, 0x13, 0x2A)

# The mark, one character a cell: i the stem and the flag (the ink), a the head (the accent), h the
# head's highlight, a lighter pixel.
MARK = [
    "...ii..",
    "...iii.",
    "...i.ii",
    "...i..i",
    "...i...",
    ".aaa...",
    "ahaa...",
    "aaa....",
]

# Each icon grid: its size, the mark's cell in pixels, where the mark's corner lies, and how far
# each of the first rows of the tile is cut in from the side (the same on the four corners).
GRIDS = {
    16: dict(cell=1, at=(4, 4), cuts=[2, 1]),
    24: dict(cell=2, at=(5, 4), cuts=[2, 1, 1]),
    32: dict(cell=3, at=(5, 4), cuts=[3, 2, 1, 1]),
}
# The size of each PNG, and the grid it is cut from.
SIZES = {16: 16, 32: 32, 48: 24, 64: 32, 128: 32, 256: 32, 512: 32}


def mix(a, b, t):
    return tuple(int(x + (y - x) * t + 0.5) for x, y in zip(a, b))


def hexa(colour):
    return "#%02X%02X%02X" % colour


def runs(cells):
    """The rectangles (x, y, w, h) of each key of a grid of keys (None is empty): the runs of a
    row, joined with the identical runs of the row below."""
    out = {}
    above = {}
    for y, row in enumerate(cells):
        here = {}
        x = 0
        while x < len(row):
            key = row[x]
            if key is None:
                x += 1
                continue
            end = x
            while end < len(row) and row[end] == key:
                end += 1
            item = (key, x, end - x)
            rect = above.get(item)
            if rect is None:
                rect = [x, y, end - x, 0]
                out.setdefault(key, []).append(rect)
            rect[3] += 1
            here[item] = rect
            x = end
        above = here
    return {key: [tuple(rect) for rect in rects] for key, rects in out.items()}


def svg(width, height, view, body):
    return (
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 %s" width="%d" height="%d" '
        'shape-rendering="crispEdges">\n%s</svg>\n' % (view, width, height, body)
    )


def group(attrs, rects):
    lines = "".join(
        '  <rect x="%d" y="%d" width="%d" height="%d"/>\n' % r for r in rects
    )
    return " <g %s>\n%s </g>\n" % (attrs, lines)


def mark_cells(keep):
    return [[keep.get(ch) for ch in row] for row in MARK]


def write_marks():
    out = ROOT / "assets/brand"
    out.mkdir(parents=True, exist_ok=True)
    width, height = len(MARK[0]), len(MARK)
    view = "%d %d" % (width, height)

    one = runs(mark_cells({"i": "ink", "a": "ink", "h": "light"}))
    body = group('fill="currentColor"', one["ink"])
    body += group('fill="currentColor" fill-opacity=".5"', one["light"])
    (out / "mark.svg").write_text(svg(width * 8, height * 8, view, body))

    accent = runs(mark_cells({"i": "ink", "a": "head", "h": "light"}))
    body = group('fill="currentColor"', accent["ink"])
    body += group('fill="%s"' % hexa(ACCENT), accent["head"])
    body += group('fill="%s"' % hexa(mix(ACCENT, (255, 255, 255), 0.5)), accent["light"])
    (out / "mark-accent.svg").write_text(svg(width * 8, height * 8, view, body))


def tile_grid(size):
    """The tile on a grid of size x size pixels: rows of RGB, None where it is transparent."""
    pixels = [[ACCENT] * size for _ in range(size)]
    for row, cut in enumerate(GRIDS[size]["cuts"]):
        for y in (row, size - 1 - row):
            for x in range(size):
                if x < cut or x >= size - cut:
                    pixels[y][x] = None
    return pixels


def mark_grid(size):
    """The mark on the same grid, on nothing."""
    spec = GRIDS[size]
    cell, (ox, oy) = spec["cell"], spec["at"]
    pixels = [[None] * size for _ in range(size)]
    ink = {"i": PAPER, "a": NIGHT, "h": mix(PAPER, NIGHT, 0.5)}
    for r, line in enumerate(MARK):
        for c, ch in enumerate(line):
            if ch in ink:
                for y in range(oy + r * cell, oy + (r + 1) * cell):
                    for x in range(ox + c * cell, ox + (c + 1) * cell):
                        pixels[y][x] = ink[ch]
    return pixels


def icon_grid(size):
    """The icon: the mark on the tile."""
    return [
        [m or t for t, m in zip(tile_row, mark_row)]
        for tile_row, mark_row in zip(tile_grid(size), mark_grid(size))
    ]


def write_icon_svg():
    size = 32
    body = ""
    # The tile whole, the mark over it: the tile is not cut around the mark.
    for layer in (tile_grid(size), mark_grid(size)):
        for colour, rects in runs(layer).items():
            body += group('fill="%s"' % hexa(colour), rects)
    path = ROOT / "packaging/linux/icons/hicolor/scalable/apps/gamme.svg"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(svg(size, size, "%d %d" % (size, size), body))


def png(rows):
    """A PNG of rows of RGBA bytes."""

    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    height, width = len(rows), len(rows[0]) // 4
    raw = b"".join(b"\x00" + row for row in rows)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def write_icon_pngs():
    for size, grid_size in SIZES.items():
        scale = size // grid_size
        assert scale * grid_size == size, (size, grid_size)
        rows = []
        for line in icon_grid(grid_size):
            row = b"".join(
                bytes(colour + (255,)) * scale if colour else bytes(4) * scale
                for colour in line
            )
            rows.extend([row] * scale)
        path = ROOT / ("packaging/linux/icons/hicolor/%dx%d/apps/gamme.png" % (size, size))
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(png(rows))


if __name__ == "__main__":
    write_marks()
    write_icon_svg()
    write_icon_pngs()
