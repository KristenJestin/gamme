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

**The icon is a placeholder** (`packaging/linux/icons/hicolor/scalable/apps/gamme.svg`): the mark,
the icon and the colours are decided with the owner, and replacing that file is all it takes. The
window itself takes the design system's palette, or the desktop's.

## Building

```
nice -n 19 cargo build --release
```

Rust 1.95 or newer and a C++ compiler: Gamme engraves scores with [Verovio](https://www.verovio.org),
whose C++ takes about 35 minutes to compile the first time. `AGENTS.md` says how to work on it.

## Licence

AGPL-3.0-only (see `LICENSE`). The bundled Geist and Geist Mono fonts, from the design system,
are under the SIL Open Font License (`assets/fonts/OFL.txt`). Verovio is LGPL-3.0-or-later.
