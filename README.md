# Gamme

*Learn the piano: the score first, the falling notes as a help that fades.*

Gamme is a native app for Linux, in Rust on GPUI, to learn the piano with a MIDI keyboard. The
score stays at the centre; falling notes over a keyboard are an aid that goes away as you master a
piece, because apps that only show falling notes teach to follow, not to read.

**Status: the skeleton.** What exists today is one window with the shared design system's look
(it follows the Omarchy theme), a home screen that lists your pieces, and the machinery to
publish, install and update the app. The play screen, the game layer, the practice method and the
fingering come next.

## The library

Import a piece with the **Import** button, or drop the file on the window: MusicXML (`.mxl`,
`.musicxml`) and MEI are engraved, MIDI (`.mid`, `.midi`) has falling notes only. The file is
copied, with its title and composer, into `$XDG_DATA_HOME/gamme/library` (`~/.local/share/gamme/library`
by default). The home screen lists the pieces, the last played first; a piece is removed from the
"⋯" menu of its row, which leaves the original file alone. A file that is not a score is refused
with a sentence that says why.

## Install

```
curl -fsSL https://raw.githubusercontent.com/KristenJestin/gamme/main/install.sh | sh
curl -fsSL https://raw.githubusercontent.com/KristenJestin/gamme/dev/install.sh | sh -s -- --channel beta
```

No Rust and no root: Linux x86_64 with `curl`, `tar`, `sha256sum` and GNU `sort`. It puts `gamme`
in `~/.local/bin` and adds Gamme, with its icon, to the application launcher. `main` has an
`install.sh` once the first stable release is out; until then take `dev`'s. The window needs the
system's fontconfig, freetype, xkbcommon (and xkbcommon-x11), wayland-client or libxcb, and a
Vulkan loader with a driver.

```
gamme --version            # the version, the channel and the commit
gamme update               # the latest build of the channel you are on
gamme update --channel beta
gamme update --channel alpha --branch feat/2-play
```

Releases, channels, `install.sh` and `gamme update` are described in
[`docs/releases.md`](docs/releases.md).

## Brand

**The mark is a pixel note**, an 8-bit eighth note on a grid of 7 by 8 cells, crisp down to 16
pixels (`assets/brand/mark.svg`, in one colour, and `mark-accent.svg`, its head in the accent).
The icon is that note on a violet tile (`packaging/linux/icons/`: an SVG and a PNG at 16, 32, 48,
64, 128, 256 and 512 pixels, each cut from the pixel grid without smoothing; 16 and 48 have grids
of their own). `python3 tools/brand.py` writes all of them, so they never drift apart.

The colours are **Nocturne**, as a theme of the design system (`src/theme.rs`), light and dark:

| | |
|---|---|
| Accent, the primary | `#7C5CFF` violet |
| Second, the running state | `#20B2C8` cyan |
| Hit, a note that was played | `#FF7AB6` pink (the kit has no role for it yet) |
| Paper, the light page | `#F6F5FB` |
| Night, the dark page | `#13132A` |

On the home screen, **Colours** chooses where the window's colours come from, as it does in the
other applications on the design system: Omarchy (the desktop's theme, followed as it changes; the
default) or Gamme (Nocturne, with a **Shade** of its own, light or dark). Off Omarchy the window
shows Nocturne and the choice says why Omarchy is not available. The choice applies at once and
is kept in `$XDG_STATE_HOME/gamme/appearance.json`. The settings page will take the control over.

The style is pixel art everywhere except the score, which stays engraved; the pixel-art
interface is its own issue.

## Building

```
nice -n 19 cargo build --release
```

Rust 1.95 or newer and a C++ compiler: Gamme engraves scores with [Verovio](https://www.verovio.org),
whose C++ takes about 35 minutes to compile the first time. `AGENTS.md` says how to work on it.

## Licence

AGPL-3.0-only (see `LICENSE`). The bundled Geist and Geist Mono fonts, from the design system,
are under the SIL Open Font License (`assets/fonts/OFL.txt`). Verovio is LGPL-3.0-or-later.
