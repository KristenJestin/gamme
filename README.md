# Gamme

*Learn the piano: the score first, the falling notes as a help that fades.*

Gamme is a native app for Linux, in Rust on GPUI, to learn the piano with a MIDI keyboard. The
score stays at the centre; falling notes over a keyboard are an aid that goes away as you master a
piece, because apps that only show falling notes teach to follow, not to read.

**Status: first notes.** What exists today is one window with the shared design system's look
(it follows the Omarchy theme), a home screen that lists your pieces, a play screen that shows a
piece engraved and falling over a keyboard and plays it with a built-in piano or a MIDI output,
your MIDI keyboard lighting and sounding its keys, practice with one hand or both (the piece
waiting for you, or looping over a few measures), every note you play judged as you play it, with a
combo and a score, and the machinery to publish, install and update the app. The rest of the game
layer, the practice method and the fingering come next.

## The library

Import a piece with the **Import** button, or drop the file on the window: MusicXML (`.mxl`,
`.musicxml`) and MEI are engraved, MIDI (`.mid`, `.midi`) has falling notes only. The file is
copied, with its title and composer, into `$XDG_DATA_HOME/gamme/library` (`~/.local/share/gamme/library`
by default). The home screen lists the pieces, the last played first; a click on one opens it to
play, and a piece is removed from the "⋯" menu of its row, which leaves the original file alone.
A file that is not a score is refused with a sentence that says why.

## Playing a piece

The play screen shows the piece in one of three views: **Score and notes** (the default: the
score, its cursor and the notes that sound, over the falling notes), **Falling notes** and
**Score**. The views share one clock: changing the view keeps the place. A MIDI piece has no
score, so it shows the falling notes only.

The notes fall toward the keys that play them, three seconds of the piece ahead, the right hand
in violet and the left in cyan; a key lights in its hand's colour while the piece plays it.
Repeats are played through, and the score's cursor goes back with them. The transport under the
piece plays and pauses, moves by measure and slows the piece down to 25 %. Until the piano is
ready (its first download), or with no audio output, the piece goes on in silence.

Hands come from the score's staves: the upper staff is the right hand, the lower the left. A MIDI
file has no staves: its tracks (or the channels of a track) are given a hand by their names, or
else by their pitch, the higher ones the right hand. **Hands** corrects them, part by part (right,
left, or both split at the middle C), and the choice is kept with the piece.

Keys: `Space` plays and pauses, `v` changes the view, `←` and `→` move by measure, `+`, `-` and
`0` zoom the score, `h`, `w`, `a`, `b` and `l` practise (below), `Escape` goes back to the pieces.

## Practice

The row under the transport chooses what you play:

- **You play**: **Listen** (the computer plays the piece), **Right hand**, **Left hand** or **Both
  hands** (`h` goes round them). The computer plays the hand you do not, and never yours: your
  hand's notes still fall and light, but only your keyboard sounds them.
- **Wait for me** (`w`): the piece stops at each note or chord of your hands until you strike it,
  every key of it down together, from your MIDI keyboard. The computer's hand waits with it and
  plays on with your chord. A wrong key releases nothing, and a key you held from before is struck
  again. Striking up to 200 ms (of real time) before the chord counts, so that being a little
  ahead does not make you play it twice; earlier than that counts for nothing.
- **The loop**: **A** (`a`) and **B** (`b`) set its first and last measures where the piece is
  (move there with `←` and `→`), **Clear** (`l`) ends it. Once both are set, measures A to B play
  again and again: at B's end the piece goes on from A's start with no gap, the notes still
  sounding let go. The loop counts measures in the order they are played, so after a repeat a
  measure number comes twice: the control names the measures as the score prints them, and as
  played when a repeat makes the two differ. Made while the piece is outside it, the loop starts
  at A.

What you play and the wait are kept with the piece; the loop is not.

Known limits:

- the wait listens to a MIDI keyboard only (the keys on the screen cannot be clicked): with none
  plugged in, or while you listen, it cannot be turned on and the piece does not wait;
- a chord is the notes of your hands that start within 30 ms of each other: a rolled chord written
  as separate notes is waited for note by note.

## Judgements

While you play a hand, every note of it is judged by when you strike its key, in real time
(whatever the speed):

| Judgement | When |
|---|---|
| perfect | within 50 ms of the note, early or late |
| good | within 120 ms, early or late |
| late | 120 to 250 ms after |
| missed | not struck within 250 ms |

A key struck where none of your notes is due is **wrong**. Each note of a chord is judged on its
own; a key repeated fast goes to each of its notes in turn. With **Wait for me** on there is no
timing: a note is perfect when you strike it, a wrong key is wrong. Each time the loop comes round,
or after a seek, the notes are owed again and judged afresh; changing the hands you play, or
playing again after the end, starts the count again. Under the wait, a key you strike again
because your chord was not all down at once is not counted against you. The latency measured in
the settings is taken away from each key.

Each judgement shows as it is made, for a moment, in its colour: on the falling note and on its
key (perfect pink, good green, late amber, missed grey), with its word rising over the key, and
around the note on the score. A wrong key flashes red on the key. A miss is said once the note's
time is past, so it shows a little after the note has gone by.

The row under the transport keeps the run's account:

- **Score**: a note is worth 100 when perfect, 70 when good, 30 when late, nothing when missed,
  times the combo, up to ×10.
- **Combo**: the notes struck in a row, back to 0 at a note missed or a wrong key; a late note
  keeps it going.
- **Accuracy**: the share of the notes judged that were perfect or good. A wrong key is no note:
  it breaks the combo but leaves the accuracy.
- **Wrong**: the wrong keys.

Under **Wait for me** there is no timing, so the notes you play there build the combo but earn no
points and stay out of the accuracy: the row says how many were played with the wait, not
scored.

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
gamme piece.musicxml       # opens a piece without importing it
gamme piece.mxl --at 30 --play --view falling   # from 0:30, playing, falling notes only
gamme update               # the latest build of the channel you are on
gamme update --channel beta
gamme update --channel alpha --branch feat/2-play
```

The first piece opened unpacks Verovio's music fonts (about 12 MB) into `$XDG_CACHE_HOME/gamme`
(`~/.cache/gamme`), once.

Releases, channels, `install.sh` and `gamme update` are described in
[`docs/releases.md`](docs/releases.md).

## The built-in piano

Gamme plays the piece itself, with a SoundFont synthesizer on the default audio output. The
SoundFont is **not bundled**: it is downloaded the first time you play, into
`$XDG_DATA_HOME/gamme/soundfonts` (`~/.local/share/gamme/soundfonts`), with its progress shown. The
download is resumable (HTTP `Range` on a `.part` file, using your `curl`), and checked by its size
and SHA-256 before anything is unpacked.

| Piano | Download | On disk | Licence |
|---|---|---|---|
| Upright piano, small (the default) | 5.8 MiB (`.7z`) | 9 MiB | CC0 1.0 |
| Concert grand (Salamander Grand Piano) | 296 MiB (`.tar.xz`, unpacked by your `tar`) | 1.2 GiB, all of it in memory once played | CC BY 3.0 |

`gamme play <piece> [--piano small|grand]` opens a piece with the piano of your choice.

## Your MIDI keyboard

Gamme listens to every MIDI input port as it appears, a keyboard plugged in while the window is
open included, and again after it was unplugged and plugged back; or only to the port chosen in the
settings. On the play screen the keys you strike light up on the keyboard (in the hit colour) and
sound on the piece's output, whether the piece plays or not, with your sustain pedal: the built-in
piano, at the shortest buffer the audio device takes (256 frames, about 5 ms, when it can), or the
MIDI output. On the built-in piano your keys and pedal are apart from the piece's notes: a key you
hold is not cut by the piece ending its own note on that key, by a pause or by a seek, a key you
let go does not cut the piece's note on it, and your pedal holds your notes only. The keys held
and the pedal down on a keyboard that is unplugged are let go, within a second. Each key and each
move of the pedal is stamped with the instant it arrived: the judge places each key on the
piece's clock at that instant (Judgements, below).

The built-in piano is opened with the piece when its SoundFont is there already; otherwise the
first key you strike (or **Play**) downloads it. A digital piano that is both your keyboard and the
MIDI output sounds its own keys: they are not sent back to it. Gamme's own ports are never
listened to, and ALSA's **Midi Through** only when it is the port chosen.

Known limits:

- a keyboard unplugged is seen within a second, so its keys and pedal are let go up to a second
  late; one unplugged and plugged back within that second may stay silent until it is unplugged
  longer (or the input is chosen again in the settings);
- on a MIDI output your sustain pedal goes on the piece's channel, so while you hold it the
  piece's notes ring on too, through a pause or a seek, as they would on the piano itself.

## MIDI out and the settings

The piece can sound on a **MIDI output** (a piano with its own speakers) instead of the built-in
piano: the notes the computer plays go to the port, timed by the piece's clock. Whatever sounds is
let go on a pause, a seek, the end of the piece and when the port is changed (a note-off for each
key, then All Notes Off). If the port is not there when the piece starts, or goes while it plays
(a USB piano unplugged), the built-in piano takes over where the piece stood and the window says so.

The **settings page** (the Settings button of the home screen) chooses:

- **MIDI**: the output the piece sounds on (the built-in piano until a port is chosen) and the
  input the keys are read from (every port, or one; the change applies at once), as ALSA lists
  them, with **Look again** to list a piano plugged in meanwhile. A port is kept by its name, not
  by the numbers ALSA changes at each start;
- **Sound**: which piano the built-in one plays (the table above), and the **touch** of your keys
  on it: *Light* (the default: soft keys sound louder), *Normal* (the velocity as you play it),
  *Heavy* (a firmer touch is needed) or *Fixed* (every key at one volume). It shapes the keys you
  strike on the built-in piano, not the piece and not a MIDI output, whose own touch setting
  applies; a value this version does not know reads as *Light*;
- **Latency**: **Calibrate** plays 16 clicks on the output the piece plays on (the built-in piano
  made ready first if it must be); you listen to the first 4, then strike any key with each click.
  The mean of how far your keys land from the clicks, early or late, is your own offset (the sound
  reaching you, your hand, the key coming back): it is kept and taken away when your keys are
  judged. Slips (more than 100 ms from the others) do not count, fewer than 6 keys measure nothing,
  and what is kept stays within ±250 ms. It needs a MIDI keyboard plugged in, and says which output
  it measured (the built-in piano when a MIDI output could not be used). A value missing, or one
  this version cannot take, reads as 0;
- **Colours**: Gamme's Nocturne or Omarchy's theme (see Brand).

The choices apply at once and are kept in `$XDG_CONFIG_HOME/gamme/settings.json` (`~/.config/gamme`,
the folder `0700`, the file `0600`, written atomically), except the colours, which stay in the state
folder. `gamme play` uses them too.

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

On the settings page, **Colours** chooses where the window's colours come from, as it does in the
other applications on the design system: Gamme (Nocturne, the default, with a **Shade** of its own, light or dark;
dark until chosen) or Omarchy (the desktop's theme, followed as it changes). Off Omarchy the
window shows Nocturne whatever is chosen, and the control says why Omarchy is not available. The
choice applies at once and is kept in `$XDG_STATE_HOME/gamme/appearance.json`.

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
are under the SIL Open Font License (`assets/fonts/OFL.txt`). Verovio is LGPL-3.0-or-later. The
SoundFonts are downloaded, not distributed with Gamme: the small upright is the FreePats project's
*Upright Piano KW* (CC0 1.0), the concert grand is Alexander Holm's *Salamander Grand Piano*,
packed by FreePats (CC BY 3.0: credit to Alexander Holm).
