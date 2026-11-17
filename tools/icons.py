#!/usr/bin/env python3
"""Writes Gamme's pixel icons (issue #29) from their grids:

    python3 tools/icons.py

Each icon is a grid of 7 x 7 cells, written to assets/icons/<name>.svg as one square a cell,
crisp edges, in the ink it is given (currentColor). The kit draws an icon 14 pixels wide, so a
cell is 2 pixels: whole pixels, like the rest of the pixel look. The names are Lucide's, the
family the kit's own icons come from: Gamme's assets serve these first (src/icons.rs), so that
the kit's buttons, selects, menus and toasts show them in place of the stroked ones.

Standard library only.
"""

from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "assets" / "icons"

# One string a row, X the ink.
ICONS = {
    # The kit's, in place of Lucide's.
    "plus": ["...X...", "...X...", "...X...", "XXXXXXX", "...X...", "...X...", "...X..."],
    "minus": [".......", ".......", ".......", "XXXXXXX", ".......", ".......", "......."],
    "settings": ["...X...", ".XXXXX.", ".XX.XX.", "XX...XX", ".XX.XX.", ".XXXXX.", "...X..."],
    "ellipsis": [".......", ".......", ".......", "X..X..X", ".......", ".......", "......."],
    "check": [".......", "......X", ".....XX", "X...XX.", "XX.XX..", ".XXX...", "..X...."],
    "x": ["X.....X", ".X...X.", "..X.X..", "...X...", "..X.X..", ".X...X.", "X.....X"],
    "chevron-down": [".......", ".......", "X.....X", ".X...X.", "..X.X..", "...X...", "......."],
    "chevron-up": [".......", "...X...", "..X.X..", ".X...X.", "X.....X", ".......", "......."],
    "chevron-left": ["....X..", "...X...", "..X....", ".X.....", "..X....", "...X...", "....X.."],
    "chevron-right": ["..X....", "...X...", "....X..", ".....X.", "....X..", "...X...", "..X...."],
    "chevrons-up-down": ["...X...", "..X.X..", ".X...X.", ".......", ".X...X.", "..X.X..", "...X..."],
    "play": ["X......", "XXX....", "XXXXX..", "XXXXXXX", "XXXXX..", "XXX....", "X......"],
    "triangle-alert": ["...X...", "..XXX..", "..X.X..", ".XX.XX.", ".XXXXX.", "XXX.XXX", "XXXXXXX"],
    "trash": ["..XXX..", "XXXXXXX", ".X...X.", ".X.X.X.", ".X.X.X.", ".X...X.", ".XXXXX."],
    "rotate-cw": ["..XXX.X", ".X...XX", "X...XXX", "X......", "X.....X", ".X...X.", "..XXX.."],
    # Gamme's own.
    "music": ["...XXX.", "...X.XX", "...X..X", "...X...", ".XXX...", "XXXX...", ".XX...."],
    "pause": [".XX.XX.", ".XX.XX.", ".XX.XX.", ".XX.XX.", ".XX.XX.", ".XX.XX.", ".XX.XX."],
    "skip-back": ["X.....X", "X...XXX", "X.XXXXX", "XXXXXXX", "X.XXXXX", "X...XXX", "X.....X"],
    "skip-forward": ["X.....X", "XXX...X", "XXXXX.X", "XXXXXXX", "XXXXX.X", "XXX...X", "X.....X"],
    "star": ["...X...", "..X.X..", "XXX.XXX", ".X...X.", "..X.X..", ".XX.XX.", "XX...XX"],
    "star-fill": ["...X...", "..XXX..", "XXXXXXX", ".XXXXX.", "..XXX..", ".XX.XX.", "XX...XX"],
    "trophy": ["XXXXXXX", "X.XXX.X", ".XXXXX.", "..XXX..", "...X...", "..XXX..", ".XXXXX."],
    "target": [".XXXXX.", "X.....X", "X.XXX.X", "X.X.X.X", "X.XXX.X", "X.....X", ".XXXXX."],
    "flame": ["...X...", "..XX...", "..XXX..", ".XXXX.X", ".XX.XXX", "XX...XX", ".XXXXX."],
    "repeat": [".XXXXXX", "X....X.", "X......", "X.....X", "......X", ".X....X", "XXXXXX."],
}


def runs(grid):
    """The rectangles (x, y, w, h) of a grid: each run of a row."""
    for y, row in enumerate(grid):
        x = 0
        while x < len(row):
            if row[x] != "X":
                x += 1
                continue
            start = x
            while x < len(row) and row[x] == "X":
                x += 1
            yield start, y, x - start, 1


def svg(grid):
    rects = "".join(f'<rect x="{x}" y="{y}" width="{w}" height="{h}"/>' for x, y, w, h in runs(grid))
    return ('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 7 7" width="14" height="14" '
            f'shape-rendering="crispEdges" fill="currentColor">{rects}</svg>\n')


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    for name, grid in ICONS.items():
        assert len(grid) == 7 and all(len(row) == 7 for row in grid), name
        (OUT / f"{name}.svg").write_text(svg(grid))
        print(OUT / f"{name}.svg")


if __name__ == "__main__":
    main()
