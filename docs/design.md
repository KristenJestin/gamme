# Gamme: design

Gamme teaches one person to play the piano at home, with a MIDI keyboard plugged into their
computer. It shows the piece both ways at once, the engraved score and the notes falling over a
keyboard, plays the part the player is not playing, listens to what they play, and judges every
note. Its aim is a player who can read and play a piece from the score alone.

This document is the reference for what Gamme is and how it is built. Each milestone (§10) has its
issues; an issue that needs more than its own text gets a document of its own in `docs/`, linked
from here.

## 1. What Gamme is, and what it is not

Gamme is:

- **a falling-notes player with a game layer**: notes fall over a keyboard toward the moment they
  are due, the player strikes them, and the game says how well, as it happens;
- **a practice method built in**: short passages, hands separately, slow, then the tempo raised
  step by step (§4);
- **a score reader in the making**: the score is always there, and the falling notes are an aid
  that fades as a piece is mastered (§4.2).

Gamme is not:

- a score editor, a recorder or a composition tool;
- an online service: no account, no leaderboard, no store. Everything stays on the machine;
- a teacher of posture, technique or expression. It says so (§4.4).

Linux only, on GPUI through `gpui-kit` and the shared `gpui-design-system`.

## 2. Vocabulary

| Word | Meaning |
|---|---|
| Piece | A score imported into the library: MusicXML or MEI (engraved), or MIDI (falling notes only). |
| Part | The notes of one hand: the right hand is the upper staff, the left hand the lower one. |
| Passage | A few consecutive measures of a piece, the unit of practice (§4.1). |
| Run | One play of a piece or a passage, from start to stop, at one speed, with one setting of the aid. |
| Wait | A setting of a run: the clock stops at each note or chord until the player strikes it. |
| Speed | The tempo of a run, from 25 % to 100 % of the piece's own. |
| Loop | A passage played again and again (A–B), with no pause between runs. |
| Judgement | What the game says of one expected note: perfect, good, late, missed; and of a key struck when nothing was expected there: wrong. |
| Aid | The falling notes, at their current strength (§4.2). |
| Step | One rung of the practice ladder of a passage (§4.1). |
| Star | A mark of a piece mastered at a speed (§5.3). |

## 3. Architecture

One process, one window. The main thread only draws and puts results in place; everything slow
runs off it.

| Part | What it does | Built on |
|---|---|---|
| Library | imports pieces, keeps their files and metadata in the data folder | the file system |
| Engraving | the score as SVG pages, the timemap (when each note sounds), the notes per staff | Verovio (`verovio` crate) |
| Rasterizing | SVG pages into images, once per size and zoom, off the main thread | `resvg` |
| Timeline | the notes of a piece in time, per hand, with their measure and their engraved element | Verovio's MIDI output and timemap |
| Clock | the play position, shared with the audio thread; stops in wait mode | atomics |
| Audio | the computer's part and the player's notes, through the built-in piano or MIDI out | `rustysynth`, `cpal`, `midir` |
| MIDI in | watches input ports, connects every one that appears, timestamps each note | `midir` |
| Judge | compares the notes played with the notes expected (§5.1) | Gamme |
| Progress | runs, best scores, stars, the ladder's state per piece | the state folder |

Files: config in `$XDG_CONFIG_HOME/gamme`, the library and the SoundFont in `$XDG_DATA_HOME/gamme`,
progress in `$XDG_STATE_HOME/gamme` (folder `0700`, files `0600`), Verovio's resources in
`$XDG_CACHE_HOME/gamme`, unpacked once.

### 3.1 What the prototype taught

A prototype proved the stack (Verovio, resvg, rustysynth, midir, wait mode). Its traps, to keep in
mind:

- **usvg's absolute bounding boxes are wrong for Verovio's SVG**: a `<use>` offset is applied twice
  and empty groups pull boxes to the origin. Compose the group transforms yourself to place a note.
- **Verovio's timemap plays repeats through with copied ids**: map them back to the engraved notes
  (`notated_id_for_element`).
- **`verovio` 0.3.6's `midi_values_for_element` re-renders the timemap at every call** (20 s for a
  rag): take the notes from Verovio's MIDI output instead (one track per staff, ties joined,
  ~135 ms).
- **`Toolkit::new` unpacks its resources into a new temporary folder it never deletes**: build one
  toolkit per process, with its resources in the cache folder, never one per call.
- Rasterizing three pages takes 100–150 ms: fine on resize or zoom, never per frame. The cursor and
  the highlights are GPUI overlays drawn every frame.
- Verovio's text font is embedded as woff2, which resvg cannot load: tempo marks lose their glyph.

## 4. How one learns with Gamme

Falling-notes apps teach note-matching and memorising, not reading: the common objection of piano
teachers, and what a Royal College of Music study of an AR piano app measured (its pupils learnt
pieces without staff notation, and their sight-reading developed more slowly). Gamme keeps their
pull (the game, the immediate answer) and works against their flaw.

### 4.1 The practice ladder

A piece is cut into passages of a few measures (by default 4, along the piece's phrases when the
score marks them). Each passage climbs the same ladder:

1. right hand, slow, wait on;
2. left hand, slow, wait on;
3. both hands, slow, wait on;
4. both hands, slow, wait off;
5. both hands, the tempo raised by steps (60 %, 75 %, 90 %, 100 %).

A step is passed when the passage is played **3 times in a row without a fault** (no missed or
wrong note; at step 4 and above, nothing late either). Below that, the passage loops at the same
step. Then passages are joined two by two, and so on up to the whole piece.

Opening a piece says where the player stands: "measures 9–12, both hands, 75 %".

### 4.2 The aid that fades

The score is always on screen. The falling notes are an aid whose strength follows the mastery of
the passage: full on the first steps, fainter and shorter as steps are passed, gone at the last.
The player can bring it back at any time; the third star is only earned without it (§5.3).

### 4.3 Fingerings

MusicXML carries fingerings; Verovio engraves them. Gamme also writes them on the falling notes,
so that the hand learns them from the start.

### 4.4 What Gamme does not teach

Posture, technique, the weight of the hand, pedalling and expression need a teacher or a good
video course. Gamme says so in its help, and does not pretend otherwise.

## 5. The game

### 5.1 Judgements

Each expected note is judged once, by the timing of the key struck for it (wait off):

| Judgement | Timing |
|---|---|
| perfect | within ±50 ms |
| good | within ±120 ms |
| late | 120–250 ms after |
| missed | not struck within 250 ms |

A key struck where no note is expected within the window is **wrong**. With wait on, there is no
timing: a note is perfect when struck, a wrong key is wrong. The judgement shows on the note
itself, on the falling note, the engraved one and the key, and is animated.

### 5.2 Score and combo

Points per judgement (perfect 100, good 70, late 30, missed and wrong 0), multiplied by the combo
(consecutive notes without missed or wrong, capped). Accuracy is the share of notes judged perfect
or good.

### 5.3 The end of a run

Score, accuracy, the stars earned, and **the measures most missed**, with "Rework": a loop on those
measures, slowed. Stars per piece: one at 50 % speed, two at 75 %, three at 100 % with the aid
gone; each needs at least 90 % accuracy with wait off.

### 5.4 Progress

Per piece: best score per speed, stars, the ladder's state, the history of runs. Kept locally,
never sent anywhere.

## 6. Playing a piece

- **Views**: score and falling notes together (the default), falling notes only, score only.
- **Hands**: listen (the computer plays all), right hand, left hand, both. The computer plays the
  part the player does not.
- **Wait**, **speed** (25–100 %), **loop** A–B on measures, seek by measure.
- **Sound**: the built-in piano, a SoundFont downloaded on first use into the data folder, with
  its progress shown (a small piano by default; a large concert grand as an option); or MIDI out
  to a piano with its own speakers.
- **MIDI in**: every input port connected as it appears; a key latency calibration in Settings.

## 7. The library

Pieces are imported by file (MusicXML `.mxl`/`.musicxml`, MEI, MIDI) or dropped on the window.
MIDI pieces have no score: falling notes only, and no star beyond the second. The home screen
lists the pieces played last, then the rest.

From v0.4, a curated selection of public-domain piano pieces from PDMX (a public-domain MusicXML
dataset drawn from MuseScore, with a complexity score per piece), sorted by level. Its licence is
checked first: its paper says both CC0 and CC-BY.

## 8. The window

Every screen is built from `gpui-design-system`'s components; a component it lacks is added to the
kit, not drawn in Gamme. Everything that moves is animated. The theme follows the desktop
(Omarchy) unless the player chooses one.

Screens: home (pieces), play, end of run, settings (MIDI in and out, sound, latency, theme).

## 9. Releases

As Flottille's: versions computed from the commits, alpha per branch push, beta per `dev` commit,
stable by the release workflow; `install.sh` and `gamme update`. See
[`releases.md`](releases.md).

## 10. Milestones

The plan is cut into GitHub milestones; each ends on something the maintainer can use, accepted on
his machine.

1. **v0.1 - Foundation**: the maintainer imports a MusicXML piece, sees it engraved and falling,
   plays it with his MIDI keyboard hands separately, with wait, slowed and looped, hearing the
   built-in piano or his own; installed by `install.sh` and updated by `gamme update`. The brand
   is chosen.
2. **v0.2 - Game**: the same piece played as a game: judged notes, combo and score, an end screen
   that sends him to the measures he missed, stars, progress kept.
3. **v0.3 - Method**: the practice ladder and the fading aid (§4); fingerings.
4. **v0.4 - Pieces**: pieces by level from the first launch (§7).
5. **v0.5 - Reading**: note-reading exercises, a daily goal and a streak. An idea, nothing
   decided.

**Feedback**, beside them: the maintainer's feedback, bugs and ideas, tied to no version.

## 11. Defaults taken, to confirm

- Judgement windows ±50 / ±120 / 250 ms (§5.1), before latency calibration.
- Passages of 4 measures, 3 clean runs to pass a step, the tempo steps of §4.1.
- Stars at 50 / 75 / 100 % speed, 90 % accuracy, wait off; the third without the aid.
- A small piano SoundFont by default, the large one as an option.
- MIDI pieces stop at two stars.
