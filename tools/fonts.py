#!/usr/bin/env python3
"""Writes the pixel fonts Gamme bundles (issue #29, README "Brand") from their upstream files:

    python3 tools/fonts.py

- assets/fonts/Jersey10-Regular.ttf and Jersey10-OFL.txt: Jersey 10, by Sarah Cadigan-Fried, as
  it is: the titles and the buttons;
- assets/fonts/GammeDot-Regular.ttf and GammeDot-OFL.txt: Gamme Dot, DotGothic16 by Fontworks
  changed for Gamme: the interface's text.

Both are under the SIL Open Font License 1.1, DotGothic16 with no Reserved Font Name, so a
Modified Version of it may be bundled; it is renamed all the same, so that it is never taken for
the original. What changes:

- "ç": DotGothic16 draws it as its "c" lifted by two pixels and one row taller, the cedilla above
  the baseline, so that it stands above every other lowercase letter ("leÇon"). Here it is the
  font's own "c" on the baseline, with the font's own cedilla (the rows under the lifted bowl)
  moved below it, into the descent.
- U+202F, the narrow no-break space French puts before : ; ! ?: DotGothic16 has none, and its
  no-break space is a whole cell wide. A blank glyph a quarter of an em wide is added.
- Only the Latin scripts, punctuation, currency, letterlike symbols and arrows are kept: the
  Japanese glyphs take 2 MB.

The upstream files are fetched at pinned commits and checked by their SHA-256. fontTools (`pip
install fonttools`) is needed here, to write the files again, never to build Gamme.
"""

import hashlib
import sys
import tempfile
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "assets" / "fonts"

# The commits Google Fonts ships (google/fonts, ofl/*/METADATA.pb).
DOTGOTHIC = "https://raw.githubusercontent.com/fontworks-fonts/DotGothic16/e44ca7bb46e7f353302c1431bf752af007c4fdfe"
JERSEY = "https://raw.githubusercontent.com/scfried/soft-type-jersey/d8446c4c9c2ba14cf408c295be35213c006e19ff"
SOURCES = {
    "DotGothic16-Regular.ttf": (f"{DOTGOTHIC}/fonts/ttf/DotGothic16-Regular.ttf",
                                "3ad9af88726d42b40f7f365f0dcac785af73cf20ea6f1d5b44e57cc21150b8f1"),
    "DotGothic16-OFL.txt": (f"{DOTGOTHIC}/OFL.txt",
                            "b6630c61ea078cacd7fabe37d14ffe557a0b45b06683374a9aa9e24262993e33"),
    "Jersey10-Regular.ttf": (f"{JERSEY}/fonts/ttf/Jersey10-Regular.ttf",
                             "db9cbd091617048a145d249daa2b815fe7083be6ab66ac26626e21a4e01c3e82"),
    "Jersey10-OFL.txt": (f"{JERSEY}/OFL.txt",
                         "436d5836064cec796c7ddde6fb4faeabdb6f2f17443a24a30d0c38b75d13f922"),
}

FAMILY = "Gamme Dot"
POSTSCRIPT = "GammeDot-Regular"
# DotGothic16 draws on 16 pixels to the em.
GRID = 16
# Basic Latin, Latin-1, Latin Extended-A, general punctuation (… ’ “ ” and U+202F), currency,
# letterlike symbols, arrows.
UNICODES = [*range(0x20, 0x7F), *range(0xA0, 0x180), *range(0x2000, 0x2070),
            *range(0x20A0, 0x20C0), *range(0x2100, 0x2150), *range(0x2190, 0x2200)]


def fetch(folder):
    """Each upstream file into `folder`, checked."""
    files = {}
    for name, (url, sha256) in SOURCES.items():
        with urllib.request.urlopen(url, timeout=60) as answer:
            data = answer.read()
        if hashlib.sha256(data).hexdigest() != sha256:
            raise SystemExit(f"fonts.py: {name} is not the file pinned ({url})")
        path = folder / name
        path.write_bytes(data)
        files[name] = path
    return files


def pixels(font, name):
    """The glyph's filled pixels, as (column, row), row 0 on the baseline."""
    from fontTools.pens.recordingPen import DecomposingRecordingPen

    px = font["head"].unitsPerEm / GRID
    glyphs = font.getGlyphSet()
    pen = DecomposingRecordingPen(glyphs)
    glyphs[name].draw(pen)
    polygons, current = [], []
    for op, args in pen.value:
        if op == "moveTo":
            current = [args[0]]
        elif op == "lineTo":
            current.append(args[0])
        elif op in ("closePath", "endPath"):
            polygons.append(current)
        else:
            raise SystemExit(f"fonts.py: {name} has a curve, it is not a pixel glyph")

    def inside(x, y):
        odd = False
        for polygon in polygons:
            for i in range(len(polygon)):
                (x1, y1), (x2, y2) = polygon[i], polygon[i - 1]
                if (y1 > y) != (y2 > y) and x < (x2 - x1) * (y - y1) / (y2 - y1) + x1:
                    odd = not odd
        return odd

    return {(c, r) for c in range(-2, GRID) for r in range(-6, GRID + 4)
            if inside((c + 0.5) * px, (r + 0.5) * px)}


def glyph(font, cells):
    """A glyph of whole pixels: one rectangle for each run of a row."""
    from fontTools.pens.ttGlyphPen import TTGlyphPen

    px = font["head"].unitsPerEm / GRID
    pen = TTGlyphPen(None)
    for row in sorted({r for _, r in cells}):
        columns = sorted(c for c, r in cells if r == row)
        start = previous = columns[0]
        for c in columns[1:] + [None]:
            if c is not None and c == previous + 1:
                previous = c
                continue
            x0, x1, y0, y1 = start * px, (previous + 1) * px, row * px, (row + 1) * px
            pen.moveTo((x0, y0))
            pen.lineTo((x0, y1))
            pen.lineTo((x1, y1))
            pen.lineTo((x1, y0))
            pen.closePath()
            if c is not None:
                start = previous = c
    return pen.glyph()


def gamme_dot(source, out):
    from fontTools import subset
    from fontTools.pens.ttGlyphPen import TTGlyphPen
    from fontTools.ttLib import TTFont

    # The upstream file's dates are kept: the same source gives the same file.
    font = TTFont(source, recalcTimestamp=False)
    cmap = font.getBestCmap()

    # "ç": the "c", and under it what the old "ç" holds below its lifted bowl.
    c, old = pixels(font, cmap[ord("c")]), pixels(font, cmap[ord("ç")])
    lift = next(d for d in range(5) if {(x, r + d) for x, r in c} <= old)
    if lift == 0:
        raise SystemExit("fonts.py: the \"ç\" of this DotGothic16 is not lifted any more")
    bottom = min(r for _, r in c)
    cedilla = {(x, r - lift) for x, r in old if r < bottom + lift}
    name = cmap[ord("ç")]
    new = glyph(font, c | cedilla)
    new.recalcBounds(font["glyf"])
    font["glyf"][name] = new
    font["hmtx"][name] = (font["hmtx"][name][0], new.xMin)

    if 0x202F not in cmap:
        font["glyf"]["uni202F"] = TTGlyphPen(None).glyph()
        font["hmtx"]["uni202F"] = (font["head"].unitsPerEm // 4, 0)
        if "vmtx" in font:  # a CJK font has vertical metrics too: a space's
            font["vmtx"]["uni202F"] = font["vmtx"][cmap[0x20]]
        # The glyf table may share the font's glyph order, and have added the name already.
        if "uni202F" not in font.getGlyphOrder():
            font.setGlyphOrder(font.getGlyphOrder() + ["uni202F"])
        for table in font["cmap"].tables:
            if table.isUnicode():
                table.cmap[0x202F] = "uni202F"

    for record in font["name"].names:
        if record.nameID in (1, 16):
            record.string = FAMILY
        elif record.nameID == 4:
            record.string = f"{FAMILY} Regular"
        elif record.nameID == 6:
            record.string = POSTSCRIPT
        elif record.nameID == 3:
            record.string = f"{POSTSCRIPT};modified for Gamme"
        elif record.nameID == 5:
            record.string = (f"{record.toUnicode()}; modified for Gamme: ç on the baseline, "
                             "U+202F, Latin only")
    for nid in (21, 22):
        font["name"].removeNames(nameID=nid)

    options = subset.Options()
    options.layout_features = ["*"]
    options.name_IDs = ["*"]
    options.name_languages = ["*"]
    options.notdef_outline = True
    options.glyph_names = True
    cut = subset.Subsetter(options)
    cut.populate(unicodes=UNICODES)
    cut.subset(font)
    font.save(out)


def main():
    try:
        import fontTools  # noqa: F401
    except ImportError:
        raise SystemExit("fonts.py: fontTools is needed (pip install fonttools)")
    OUT.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as folder:
        files = fetch(Path(folder))
        gamme_dot(files["DotGothic16-Regular.ttf"], OUT / f"{POSTSCRIPT}.ttf")
        licence = files["DotGothic16-OFL.txt"].read_text()
        (OUT / "GammeDot-OFL.txt").write_text(
            "Gamme Dot is a Modified Version of DotGothic16, made by tools/fonts.py of Gamme\n"
            "(https://github.com/KristenJestin/gamme): its \"ç\" redrawn on the baseline, U+202F\n"
            "added, cut to the Latin scripts, renamed. It is under the same licence.\n\n"
            + licence)
        (OUT / "Jersey10-Regular.ttf").write_bytes(files["Jersey10-Regular.ttf"].read_bytes())
        (OUT / "Jersey10-OFL.txt").write_bytes(files["Jersey10-OFL.txt"].read_bytes())
    for name in ("GammeDot-Regular.ttf", "GammeDot-OFL.txt", "Jersey10-Regular.ttf", "Jersey10-OFL.txt"):
        print(OUT / name)


if __name__ == "__main__":
    sys.exit(main())
