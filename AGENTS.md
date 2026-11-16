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

Today it is the skeleton: one window, the design system's identity and theme (Omarchy's, followed),
an empty home screen, and the machinery above. The play screen, the game layer, the method, the
fingering and the pieces come as issues of their own.

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
- **Keep `gpui-fast` at 0.1.10 in `Cargo.lock`** (`cargo update -p gpui-fast --precise 0.1.10`
  after any update): 0.1.11 breaks `gpui-base` 0.8.

## Where things live

`src/` is flat, one module per file:

| File | What it is |
|---|---|
| `main.rs` | The entry: arguments (`--help`, `--version`, `update`), the window and its theme |
| `home.rs` | The home screen |
| `icons.rs` | The icons Gamme adds to the kit's (Lucide) |
| `update.rs` | `gamme update`: the channel's latest build, its sum checked, the binary replaced |
| `desktop.rs` | The launcher entry and the icon, written by `update` as `install.sh` writes them |
| `paths.rs` | `$XDG_STATE_HOME/gamme` and `$XDG_DATA_HOME` |

Besides `src/`:

- `build.rs`: the version `gamme --version` prints (`docs/releases.md`, "The version").
- `packaging/linux/`: the desktop entry and the icon (**a placeholder**: the brand is decided
  with the owner, never invented by an agent), installed by `install.sh` and packaged by
  `tools/package.sh`.
- `tools/`:
  - `capture.sh`: the window captured into a PNG, in a nested headless Weston; `xkeys.py` types
    keys and clicks into its display;
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
- **Gamme's state goes only to `$XDG_STATE_HOME/gamme`**, the folder `0700`, its files `0600`.
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
