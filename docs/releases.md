# Releases

Every branch that matters publishes its own build, with a version computed from its commits,
never typed. CI makes the tags and the releases itself: no tag is pushed by hand. It follows
Flottille's (KristenJestin/flottille, `docs/releases.md`).

| Branch | Channel | Version (example) | Published as |
|---|---|---|---|
| `main` | stable | `0.2.0` | release `v0.2.0`, latest |
| `dev` | beta | `0.2.0-beta.12` | pre-release `v0.2.0-beta.12` |
| `feat/*`, `fix/*` | alpha | `0.2.0-alpha.feat-12-search.4` | a pre-release per push, all deleted with the branch |

Nothing is published anywhere but GitHub's releases of the repository. A published version is
never replaced, its tag never moved nor its files changed: each build is its own release, an
alpha's too.

## Branches

`dev` is the default and integration branch. Branches start from `origin/dev` and reach it by
squash. `main` only gets releases, and stays an ancestor of `dev`.

## First publication

GitHub runs a branch's `delete` workflow, and lets a workflow be started by hand, from the
default branch's own files only. While `main`, which has neither, is the default,
`alpha-cleanup.yml` does not run when a branch is deleted, so its alphas stay, and `release.yml`
cannot be started. So the first time, in this order:

1. Push `dev`.
2. Make `dev` the default branch on GitHub (the repository's settings, "Default branch").
3. Only then push or delete `feat/` and `fix/` branches.

## Releasing

A release does not squash nor merge: **`main` moves forward to a commit of `dev`** (a
fast-forward), and is then that very commit.

- **The pull request `dev → main` is opened for review**, and never merged with the button. The
  `release` workflow (`.github/workflows/release.yml`) closes it: started by hand
  (`gh workflow run release.yml --ref dev`, or "Run workflow" from `dev`), with the commit of
  `dev` to release as its input, a full SHA or `dev` for its head (the default). Once `main` is
  the pull request's head, GitHub shows it merged.
- **What it checks**: it runs from `dev` only; the input is a full SHA or `dev`; the commit is on
  `dev`; `main` is its ancestor. Then it moves `main` through GitHub's API, not forced, so GitHub
  refuses anything but a fast-forward. Any of these refused, nothing moves and the run fails,
  saying why. Its job runs no code of any branch: no checkout, no git, only `gh`.
- **Then the stable build**: a ref moved with the workflow's own token starts no `push` run, so
  the `release` workflow starts `ci.yml` on `main` itself. Run again when `main` is already the
  commit, it moves nothing and builds again, which publishes nothing already published.
- **The tag `vX.Y.Z` is put on the released commit**, the same on `main` and on `dev`.
- **The stable version is the one `dev` already had**: its beta's without the pre-release
  (`0.2.0-beta.12` releases `0.2.0`). Its changelog lists the commits of `dev` since the previous
  stable tag.
- A push to `main` that is not a commit of `dev` publishes nothing and fails its run.

## The version

`tools/version.sh stable|beta|alpha [--branch <name>] [--at <commit>]`, for HEAD by default:

- **It counts from the last release**: the highest stable tag `vX.Y.Z`, compared as versions,
  from its own commit on. Pre-release tags do not count. Without a stable tag, from `0.0.0` and
  the first commit. When nothing was committed since, the commit is the release's own: every
  channel exits 3, printing nothing, and CI publishes nothing (a beta or an alpha of it would be
  the next version, on the commit already released).
- **The next version** follows the Angular commits counted, merges left out: `feat` bumps the
  minor, `fix` and `perf` the patch, a `!` after the type or a `BREAKING CHANGE:` footer the
  major; nothing that bumps is the next patch. **Before 1.0, everything bumps one level less**: a
  breaking change the minor, a `feat` the patch.
- **`Release-As: X.Y.Z`**, a footer of a commit since the last release, sets the next version
  when it is above the computed one (equal changes nothing; with several, the highest wins). A
  value below it, or not `X.Y.Z`, makes `tools/version.sh` exit 1 naming the commit, and so fails
  the build. The squash that brought CI into `dev` ends with `Release-As: 0.1.0`: the first
  release is `0.1.0`.
- **First parents only**: the commits counted, the types that bump and the changelog follow the
  build's own line, a merge being one commit of it.
- **The beta and alpha numbers** count those commits: each squash into `dev` makes the next beta,
  and the first one after a release is `-beta.1`.
- **An alpha names its branch**: each byte of the name but ASCII letters, digits and dashes is a
  dash, `feat/12-search` giving `alpha.feat-12-search.<n>`. CI and its cleanup follow that one
  rule; `feat/a_b` and `feat/a-b` share their alphas. A force-push numbers its alpha by the
  commits it holds: with fewer than before, the number is lower, and `install.sh`, taking the
  highest, installs a former alpha above it. When that version is already published (as many
  commits as before, or as few as an earlier push), nothing is published: the release stays the
  former commit's until a push gives a version not yet published.

`tools/changelog.sh [--at <rev> | <range>]` lists the Angular commits since the last release,
grouped (breaking changes, features, fixes, performance, other), `- **<scope>**: <subject>`, in
the commits' own language.

`Cargo.toml` keeps its own version. CI gives the computed one to the build
(`GAMME_BUILD_VERSION` and `GAMME_BUILD_CHANNEL`, read by `build.rs`).
`gamme --version` says the version, the channel and the commit:
`gamme 0.2.0-beta.12 (beta, 3f2a9c1d7b44)`. A build CI did not make is `local`:
`gamme 0.1.0 (local, 3f2a9c1d7b44)`.

## What CI does

`.github/workflows/ci.yml` runs on every pull request, on a push to `main`, `dev`, `feat/**`
and `fix/**`, and by hand. A newer push cancels the older runs of its ref. On `dev` and `main`,
whose runs publish, a run that started is never cancelled, but one still waiting is, by GitHub,
when a newer one is queued: its commit gets no build, and the newer run publishes the tip.

- **`verify`**: `cargo fmt --check`, `cargo clippy --all-targets --locked -- -D warnings` and
  `cargo test --locked`, on Rust 1.95. No test opens a window.
- **`build`**, on a push (and on `main` when the `release` workflow starts it): with a token that
  can only read, on Ubuntu 22.04, old enough a C library (glibc 2.35) that the binary runs on
  current Arch and Debian stable. It computes the channel (`main` stable, `dev` beta, any other
  branch alpha) and the version, builds with `cargo build --release --locked`, packages with
  `tools/package.sh` and writes the notes: the version, the channel, the commit, the install
  line, the window's system libraries and the changelog. A branch whose name holds another
  character than ASCII letters, digits and `._/-` fails the run, and nothing is published: the
  name goes as it is into the notes and the API's addresses.
- **`publish`**, once `verify` and `build` passed: the only job that writes. It runs no code of
  the repository, no git, only `gh`. It reads the branch's tip again first: a branch that moved
  or was deleted publishes nothing. A version already published stays as it is; a draft left by a
  failed upload is made again. Stable releases are marked latest, the others pre-releases.

`.github/workflows/alpha-cleanup.yml` deletes a branch's alphas, with their tags, when the branch
is deleted.

## The archive

`tools/package.sh <version> <binary> <out folder>` makes `gamme-<version>-x86_64-linux.tar.gz`:
one folder `gamme-<version>/` with `gamme`, `LICENSE`, `LICENSE-fonts.txt` and `share/` (the
desktop entry and the icons of `packaging/linux`), the same bytes from the same files. Its
`.sha256` beside it is read by `sha256sum --check`. Linux x86_64 only.

## Installing

```
curl -fsSL https://raw.githubusercontent.com/KristenJestin/gamme/main/install.sh | sh
curl -fsSL https://raw.githubusercontent.com/KristenJestin/gamme/dev/install.sh | sh -s -- --channel beta
curl -fsSL https://raw.githubusercontent.com/KristenJestin/gamme/dev/install.sh | sh -s -- --channel alpha --branch feat/12-search
curl -fsSL https://raw.githubusercontent.com/KristenJestin/gamme/dev/install.sh | sh -s -- --version 0.2.0-beta.12
```

`main` has an `install.sh` once the first stable release is out; until then take `dev`'s, or
the line each release's notes give, of its own commit. From a clone, `./install.sh` does the
same. No Rust and no root are needed: `curl`, `tar`, `sha256sum`, GNU `sort` (it compares
versions) and `uname`, on Linux x86_64, all checked first. Its body is one function called on its
last line, so a download cut short runs nothing. It follows Flottille's `install.sh`.

- **Which release**, read from GitHub's API (`/releases/latest`, `/releases/tags/v<version>`,
  `/releases?per_page=100`) with `curl`, `sed`, `awk`, `tr` and `sort`, no `jq`:
  - stable (the default): the latest release;
  - `--channel beta`: the `-beta.` pre-release **published last** of the 100 newest releases,
    not the highest version; a draft is never offered;
  - `--channel alpha --branch <name>`: the **highest version** among the branch's alphas,
    `*-alpha.<slug>.<n>` of the 100 newest releases, the slug made as CI makes it and matched
    whole, so `feat-a` never takes `feat-a-b`'s. A branch without one is said.
  - `--version X.Y.Z[-pre]`: that release exactly, of any channel, checked as a version before it
    goes into an address, and refused with `--channel` or `--branch`. One that is not there is
    said with up to five close versions.

  `--branch` goes with `--channel alpha` only. An option without its value, or an unknown one,
  exits 2. A release that is not there (404), the network failing and GitHub failing are said
  apart.
- **The sum first**: the archive and its `.sha256` go to a `mktemp -d` folder removed on exit,
  and `sha256sum --check` runs before anything is installed: a mismatch installs nothing. The
  `.sha256` must be one line naming that archive, or nothing is installed either: `sha256sum`
  checks the file a line names, which could be another.
- **Installed**: the binary is written beside (`~/.local/bin/.gamme.new`), then renamed over
  `gamme`, so a window already open keeps its own file, and the former version until it is
  closed. The desktop entry and the icon go from the archive's `share/` to `$XDG_DATA_HOME`
  (`~/.local/share` by default), the entry's `Exec=` naming the binary's real path, quoted and
  escaped as the Desktop Entry spec says when it holds another character than letters, digits
  and `/._+-`. The launcher's files failing is said, not undone.
- **Then** `~/.local/bin/gamme --version` is run and printed. A binary that does not answer (a
  system library missing) stays installed, and that is said. It also says when `~/.local/bin` is
  not in `PATH`.
- **`--uninstall`** needs no clone, no archive and no network: it removes `gamme`,
  `applications/gamme.desktop` and the icon (`icons/hicolor/*/apps/gamme.svg` and `.png`), and
  keeps the config and the state.

`GAMME_RELEASES` names another root of the API than
`https://api.github.com/repos/KristenJestin/gamme`, for tests: `tools/test-install.sh` serves
fake releases and archives on `127.0.0.1` (with `python3`) and runs `install.sh` in a temporary
`HOME`, `XDG_DATA_HOME` and `TMPDIR`, under `dash` when it is installed (`TEST_SH` names another
shell).

## The launcher entry

Gamme is in the desktop's application launcher with its icon, without root, in the user's XDG
data folder (`$XDG_DATA_HOME`, `~/.local/share` by default):

- `applications/gamme.desktop`: it runs `gamme` by the installed binary's full path (so it works
  when the launcher's `PATH` lacks `~/.local/bin`; a path with a space is quoted), with
  `Terminal=false` and `StartupWMClass=gamme`, the window's app id, which is how the desktop
  finds the entry's icon for the window.
- `icons/hicolor/scalable/apps/gamme.svg`, and `icons/hicolor/<n>x<n>/apps/gamme.png` for n =
  16, 32, 48, 64, 128, 256 and 512: the pixel note on a violet tile (README, "Brand"). Both are
  written by `tools/brand.py` from one pixel grid, never by hand.

The package holds them under `share/`, from `packaging/linux/`: every file of its `icons`
folder, so a size added there is packaged, installed and updated with no other change. The entry of the package says `Exec=gamme`; `install.sh`
and `gamme update` write it with the binary's real path (symbolic links resolved, so that the two
write the same line whatever the way the home is reached). **Refusing some characters is Gamme's
choice**: the format allows a `"`, a backtick, `$`, `\` or `%` in an `Exec` line, escaped, but
Gamme writes none of them (nor a newline) rather than escape: a path that holds one gets no
entry, and the install or the update goes on and says so. When the launcher's files cannot be
written (a data folder that is read-only, a full disk), the install and the update still
succeed, since the binary is in place, and say that the entry was not written. After writing an
icon, the `icons/hicolor` folder is dated now, so that a desktop's cache of the icon theme, stale
once the folder is newer than it, is read again.

**The entry and the icon are Gamme's.** `gamme update` rewrites them from the package, so a
local edit of the entry is lost at the next update that finds it different; to start Gamme
another way, make your own entry under another name. `gamme update` writes both again when they
differ from the new package's, and points the entry's `Exec` at the binary it runs from, so a
binary that moved is followed; a build that has none yet gets them. Nothing is rewritten that is
already right. When there is nothing to update, the entry's path is still fixed if it is wrong.

## Updating

`gamme update [--channel stable|beta|alpha] [--branch <name>] [--version <X.Y.Z[-pre]>]
[--force]` replaces the binary it runs from with the channel's latest build, as `flottille update`
does (it has no rc and no next line, which Gamme does not have): by default the channel last
chosen with `--channel` (and, for alpha, its branch), else the one it was built on (a `local`
build takes stable) and, for an alpha, the branch it was built from (`GAMME_BUILD_BRANCH`, given
by CI). It reads the releases as `install.sh` does (stable: the latest release; beta: the
`-beta.` pre-release published last; alpha: the highest version of the branch's alphas), with the
user's own `curl` spawned without a shell, checks the archive's SHA-256 sum (a wrong one replaces
nothing), unpacks it with `tar` in a folder beside the binary and renames the new binary over the
old one: the file is the old one or the new one, never half of one. A window open keeps the
former binary until it is closed.

- **The channel is remembered.** `--channel` (with its branch for an alpha) is kept once the
  update succeeds, in `$XDG_STATE_HOME/gamme/update-channel.json` (`~/.local/state/gamme/`): it is
  Gamme's own memory, not a setting. A plain `gamme update` stays on it, so `--channel alpha
  --branch feat/specs` then `--channel beta` goes there and back. `install.sh` does not write
  it: after installing another channel by hand, say `--channel` once.
- **`--version <X.Y.Z[-pre]>` installs that release exactly**, of any channel, as `install.sh
  --version` does, and goes without `--channel` and `--branch`. A version that does not exist is
  refused with the nearest published ones, and nothing is downloaded. It does not touch the
  channel remembered.
- **The same version is not installed again**: it says so and replaces nothing. **An older one
  is refused**, whether it is the latest of the channel (a numbering change left it below this
  build) or the version asked for. `--force` installs either.
- A wrong use of the options exits 2, a failure 1.

`GAMME_RELEASES`, as for `install.sh`, names the API's root for tests; `tools/test-install.sh`
runs the real binary's `update` against the same fake server.

## What only a real push proves

The version and changelog scripts were tried by hand on throwaway repositories, no test of them
is committed; the archive is made from a binary, and `install.sh` runs against a fake API. What
cannot run off GitHub is the workflows themselves: a `feat/` push publishing an alpha, a second
one the next beside it; deleting the branch removing them; the first squash into `dev`
publishing `0.1.0-beta.N`; the first `release` run moving `main` with the workflow's token, the
pull request shown merged, `ci.yml` started on `main`, `v0.1.0` tagged on the commit released
and marked latest, its binary starting without Rust; a run from another branch or of a commit
not on `dev` moving nothing and saying why; GitHub's API answering as `install.sh` reads it, and
the curl lines installing the stable, a beta and a branch's alpha on a machine without Rust,
`gamme --version` printing each one's version and channel; `gamme update` doing the same from a
binary it replaces, and the first run of `ci.yml` on `dev` paying for Verovio's C++, the next
ones (and the branches, which restore `dev`'s cache) not.
