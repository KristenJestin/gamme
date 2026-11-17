# AGENTS.md — Gamme

Instructions for any coding agent working in this repository. Read this file first.
`CLAUDE.md` is a symbolic link to this file: there is one contract, not two that drift apart.

## Overview

Gamme is a native app for Linux, in Rust on GPUI, to learn the piano. The score stays at the
centre; falling notes over a keyboard are a help that fades with mastery (apps of this kind teach
to follow, not to read). It is built on the shared design system
([gpui-design-system](https://github.com/KristenJestin/gpui-design-system)), and its versions,
CI, install and update follow [Rature](https://github.com/KristenJestin/rature)'s, which follow
Flottille's.

**The [README](README.md) is the reference for behaviour**; [`docs/releases.md`](docs/releases.md)
for versions, CI, releases, install and update. This file is how to work.

Today it is one window, the design system's identity and theme (Omarchy's, followed), the home
screen and its library, the play screen (the score, the falling notes and the built-in piano), MIDI
out and in, practice (hands, wait, A–B loop), the judge of every note with its latency calibration,
its judgements shown live with a combo and a score, the end of a run (its stars, the measures most
missed and Rework), the stars and best scores kept per piece, and the machinery above. The rest of the game layer, the method, the fingering and the pieces come
as issues of their own.

## Commands

```
cargo build
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
cargo fmt --check
```

The last three are CI's `verify` job (`.github/workflows/ci.yml`), on Rust 1.95, the crate's
`rust-version`.

- **The machine is the maintainer's own, in use while you work.** One cargo at a time, at low
  priority (`nice -n 19 cargo build`), no parallel builds, no stress loops; `.cargo/config.toml`
  caps the jobs.
- **Verovio's C++ (`verovio-sys`, through the `verovio` crate) takes ~35 minutes to compile, once
  per profile.** Never clean the `target/` folder for nothing, never build the same profile
  twice from scratch. CI caches it (`docs/releases.md`).
- **Verovio finds its elements' classes by name only on the main thread** (its registry is
  `thread_local`, filled at start-up), and Gamme's toolkit lives on the engraver's thread
  (`engraver.rs`). An option that names classes, such as `svgAdditionalAttribute`, fails there
  ("ClassId for 'note' not found"): read what a note is from `get_mei` instead.
- **Keep `gpui-fast` at 0.1.10 in `Cargo.lock`** (`cargo update -p gpui-fast --precise 0.1.10`
  after any update): 0.1.11 breaks `gpui-base` 0.8.

## Where things live

`src/` is flat, one module per file:

| File | What it is |
|---|---|
| `main.rs` | The entry: arguments (`--help`, `--version`, `update`, `play`, a piece), the window and its theme |
| `screens.rs` | The window's screens and the way between them: home, a piece played and the settings; the window's toasts |
| `home.rs` | The home screen: the library's pieces, import (picker, drop), removal, and opening one |
| `library.rs` | The library in `$XDG_DATA_HOME/gamme/library`: import, list, remove, mark played, the hands and the practice kept per piece |
| `piece_file.rs` | What an imported file is: MusicXML, MEI or MIDI, its title and composer, why it is refused |
| `icons.rs` | The icons Gamme adds to the kit's (Lucide) |
| `update.rs` | `gamme update`: the channel's latest build, its sum checked, the binary replaced |
| `desktop.rs` | The launcher entry and the icon, written by `update` as `install.sh` writes them |
| `paths.rs` | `$XDG_STATE_HOME/gamme`, `$XDG_CONFIG_HOME/gamme`, `$XDG_CACHE_HOME/gamme`, `$XDG_DATA_HOME`, `$XDG_DATA_HOME/gamme` and the library's folder |
| `config.rs` | The settings in `$XDG_CONFIG_HOME/gamme/settings.json` (piano, MIDI out and in, touch, latency), written atomically, and the output they choose (`Settings::audio`) |
| `settings.rs` | The settings page, from the kit's components; the latency's calibration, played along with clicks |
| `touch.rs` | The touch of the player's keys on the built-in piano: the pure velocity curve (light, normal, heavy, fixed), applied before the keys enter `piano.rs`'s ring |
| `theme.rs` | Gamme's palette, "Nocturne", light and dark, on the kit's tokens |
| `appearance.rs` | Omarchy's colours or Gamme's: the choice, kept in the state folder, and its control |
| `engraver.rs` | The one Verovio toolkit of the process, on its own thread; its resources unpacked once into the cache; a piece's pages, timemap, MIDI and written notes |
| `score.rs` | A piece engraved: its pages, where its measures and notes sit, the cursor at each moment |
| `raster.rs` | The pages rasterized (resvg), off the main thread |
| `score_view.rs` | The score as the play screen holds it, the cursor, the sounding notes and the judgements on their notes drawn over it every frame; the `Playhead` the clock feeds |
| `timeline.rs` | The notes of a piece in time, each with its hand, its measure and its engraved note (from Verovio's MIDI, or a MIDI file's tracks), and where its measures start |
| `play_screen.rs` | The play screen: a piece opened, its three views (score and falling notes, falling notes, score), the hands of a MIDI piece's parts, the keys played (lit, sounded, kept, handed to the wait) |
| `practice.rs` | Practice: the hands the player plays (the computer plays the others), the player's chords, the judge of the wait (`Waiter`), the A–B loop chosen, or Rework in its place |
| `judge.rs` | The judge of every note (perfect, good, late, missed, wrong), pure, on the piece's time laid end to end across the loop's passes; `Live` feeds it from the clock, the run's judgements kept in order |
| `latency.rs` | The latency calibration's clicks and its pure measure, the offset the judge takes away |
| `tally.rs` | The run's score, combo and accuracy (design §5.2), pure, counted in the piece's order (a miss is said late); what the transport says of them |
| `stars.rs` | The stars a run earns (design §5.3), pure, from its `Feat` (slowest speed, accuracy, wait, whole piece, aid): for the end of a run and the progress (`progress.rs`) |
| `progress.rs` | What a piece's runs earned (design §5.4): stars (never lowered), best score per speed, the last 200 runs, one file per piece in `$XDG_STATE_HOME/gamme/progress`, read leniently and written atomically; the home screen's stars, the end's "New best" |
| `run_end.rs` | The end of a run: the measures most missed and Rework's plan (pure), and the dialog that shows them with the stars |
| `flash.rs` | The judgements on screen, fading (pure), and every colour and size they are drawn with, from the palette |
| `falling.rs` | The falling notes over the keyboard, painted every frame in one canvas, the judgements flashing on the notes and keys, their words over them |
| `keyboard.rs` | The on-screen keyboard's keys and places, and the keys lit from outside (MIDI in) |
| `wall_clock.rs` | The clock moved by the wall clock while no sound moves it (the piano not ready, no output) |
| `clock.rs` | The play position shared with the audio thread (atomics): play, pause, seek, speed; the chords it waits at and the loop it goes round (spans of measures, apart or not), obeyed by whoever steps it (`step`, `commit`); where the piece stood at a key's instant (`position_at`) |
| `soundfont.rs` | The SoundFonts of the built-in piano and their resumable, checked download |
| `piano.rs` | The built-in piano: the audio callback's logic (`Renderer`), the player's keys on their lock-free way to it (`LiveKeys`), and the `cpal` output |
| `player.rs` | A piece being played: timeline, clock, and its output (an `Audio`: the built-in piano or MIDI out) made ready on its own thread; the fallback to the built-in piano |
| `midi_out.rs` | MIDI out as an `Audio`: a thread paces the clock by the wall clock and sends the notes; `midir` is behind `Ports` and `Wire` so no test opens a port (inputs included) |
| `midi_in.rs` | MIDI in: one thread watches the input ports (every one, or the one chosen) and connects them as they appear; each key timestamped on arrival, counted per port, handed to the play screen's `Keys` |
| `transport.rs` | The transport bar (play, seek by measure, speed, the hands played, the wait, the loop, the run's score, combo and accuracy, SoundFont progress); it holds the player, its practice and the judge of the run, its tally and its flashes, and makes the end of a run |

Besides `src/`:

- `tests/fixtures/`: the small scores the library's tests import (written by hand, free to
  redistribute): one of each kind, and broken ones; and the pieces the score's and the timeline's tests read (a
  piece with repeats, a MIDI file of three tracks), written for them (CC0).
- `build.rs`: the version `gamme --version` prints (`docs/releases.md`, "The version").
- `assets/brand/`: the mark (the pixel note), alone, in one colour and in the accent.
- `packaging/linux/`: the desktop entry and the icon (the pixel note on a violet tile, an SVG and
  a PNG at each size), installed by `install.sh` and packaged by `tools/package.sh`. **The mark,
  the icon and the palette are the maintainer's decision** (README, "Brand"): an agent never
  redraws them or picks other colours.
- `tools/`:
  - `capture.sh`: the window captured into a PNG, in a nested headless Weston; `xkeys.py` types
    keys and clicks into its display;
  - `brand.py`: writes the mark's SVGs and the icon's SVG and PNGs from one pixel grid; edit the
    grid there and run `python3 tools/brand.py`, never a generated file;
  - `version.sh`, `changelog.sh`, `package.sh`: the version, the notes and the archive of a
    build (`docs/releases.md`);
  - `test-install.sh`: `install.sh` and `gamme update` run against fake releases from a local
    server, in temporary `HOME` and XDG folders;
  - `commit-message.sh`, `branch-guard.sh`: the checks of `.githooks/` (see Git and pull
    requests).
- `.github/workflows/`: `ci.yml`, `release.yml`, `alpha-cleanup.yml` (`docs/releases.md`) and
  `pull-request.yml` (the `shape` job).

## Rules of the code

- **Every screen is built from the design system's components.** A second, slightly different
  version of one of them is a defect: when something is missing, it is added to the kit (or to a
  module of `src/` that says it is waiting to be, as Rature's `kit.rs`), never drawn by hand.
- **Slow work stays off the main thread**; the main thread only puts the result in place.
- **The audio callback never blocks, locks or allocates** (`piano.rs`): it talks to the window
  through atomics (`clock.rs`) and owns its buffers.
- **Gamme's state goes only to `$XDG_STATE_HOME/gamme`**, the folder `0700`, its files `0600`.
  The library of pieces is data, not state: `$XDG_DATA_HOME/gamme/library`.
  What it downloads or keeps (the library, the SoundFonts) goes only to `$XDG_DATA_HOME/gamme`,
  with the same modes; its configuration to `$XDG_CONFIG_HOME/gamme`.
- **Linux only.**
- Code, identifiers, comments and docs are in English. Rust 2024. A comment says why, not what.
- Touch only what the task needs; match the existing style.
- A new dependency of weight is said in the pull request.

## Testing

- **Tests never use the real config, state or home**: a temporary `HOME` and temporary XDG
  folders.
- **Never open a window on the user's screen.** No test opens one (GPUI's tests use its own test
  platform). A screenshot, or a look at the running window, goes through `tools/capture.sh`: a
  nested headless Weston with its own bus, `HOME` and XDG folders, never the user's session.

## How the work is organised

| Who | What |
|---|---|
| The maintainer | decides what is built, tries each milestone on their machine, and merges what they have not delegated |
| The lead agent | turns the maintainer's decisions into issues, launches the developing agents, reviews their work, keeps the tracker honest, and never hides a failure |
| Developing agents | implement one issue each, with its verification and a pull request; they never merge |
| CI | `verify`, `commit-messages` and `shape` on every pull request |

- **Issues are the tickets, and a ticket stands alone**: the what, the where, the "how to verify"
  and the base branch, with no local path. If one of them is missing, ask rather than guess.
- **`dev` is the integration branch and the default branch**; `main` holds released versions.

## Git and pull requests (non-negotiable)

`git config core.hooksPath .githooks`, once per clone: the `commit-msg` hook runs
`tools/commit-message.sh`, the `pre-commit` hook `tools/branch-guard.sh`.

- **Never commit, merge, rebase, push or force-push on `main` or `dev` directly.** On one of
  them, create a branch first. The `pre-commit` hook refuses it.
- **A branch starts from `origin/dev`**, as `feat/<n>-<topic>` or `fix/<n>-<topic>` (`<n>` the
  issue), and its pull request targets `dev`. **These two prefixes are the only ones**, whatever
  the commit's type: a documentation change goes on `feat/<n>-<topic>` or `fix/<n>-<topic>`, and
  its squash keeps its `docs(...)` type.
- Never rewrite published history. **No `--no-verify`.**
- **One commit = one intent.** No `wip` commits. Don't mix formatting and logic in one commit.
- The author is the maintainer's, whoever wrote the change. **No `Co-Authored-By` trailer and no
  mention of an agent** in a commit, an issue or a pull request.
- The subject is `<type>(<scope>): <subject>`: the **scope is required**, the subject is 72
  characters at most, starts lowercase, has no final period, and the type is one of `feat`,
  `fix`, `refactor`, `test`, `docs`, `chore`, `build`, `ci`, `perf`. `tools/commit-message.sh` is
  the judge (the `commit-msg` hook and CI's `commit-messages` job run it; `--range
  origin/dev..HEAD` checks a branch). It reads the scopes from the line below.
- Scopes: `repo`, `ci`, `app`, `ui`, `score`, `audio`, `midi`, `play`, `game`, `library`,
  `install`.
- **Subjects and bodies are in English**: the release notes are made from them.
- **Pull requests into `dev` are merged by squash only**, by the lead. The title is a plain
  Angular subject, since it becomes the commit; the description is written in the first person
  as the maintainer and ends with `Closes #<n>`. CI's `shape` job (`pull-request.yml`) is red
  without it, and copies the issue's labels and milestone onto the pull request.
- **`main` gets no squash and no merge commit**: the release pull request `dev -> main` is opened
  for review, and the `Release` workflow (`release.yml`, run from `dev`), not the merge button,
  closes it by moving `main` forward to a commit of `dev`. Only that workflow moves `main`.
- The versions, the alphas and betas, the first publication and the releases are in
  `docs/releases.md` ("Branches", "First publication", "Releasing"). In short: each push to a
  `feat/` or `fix/` branch publishes its alpha, each squash into `dev` a beta; no `feat/` or
  `fix/` branch is pushed before `dev` is GitHub's default branch; no tag is pushed by hand; the
  version is computed from the commits' types (`feat` bumps the minor, `fix` and `perf` the
  patch, a `!` after the type or a `BREAKING CHANGE:` footer the major, each one level less
  before 1.0); the squash that brings the CI into `dev` ends with `Release-As: 0.1.0`.

## Public repository

This repository is public. No private path, host, account or token goes into the code, an
issue, a pull request, a commit or an example. Examples use invented data.

## When done

Run the four checks of Commands (`cargo build`, `cargo test --locked`,
`cargo clippy --all-targets --locked -- -D warnings`, `cargo fmt --check`), plus the checks the
issue asks for, and report their real output. If something fails, say so; don't claim green.
