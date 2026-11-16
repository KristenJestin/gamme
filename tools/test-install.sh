#!/usr/bin/env bash
# Runs install.sh, then `gamme update` (the real binary, built beforehand), against fake
# releases, never GitHub's: a small local server on 127.0.0.1 (python3) answers the API and serves
# fake archives, named to both by GAMME_RELEASES. HOME, XDG_DATA_HOME, XDG_STATE_HOME and TMPDIR
# are temporary folders, never yours. A fake archive holds a `gamme` script that answers
# --version: no build.
#   tools/test-install.sh          GAMME_BIN names another binary than target/{debug,release}/gamme
# TEST_SH is the shell install.sh runs under: dash when it is installed, sh otherwise.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
script="$root/install.sh"
if [ -n "${TEST_SH:-}" ]; then
  read -ra shell <<<"$TEST_SH"
elif command -v dash >/dev/null 2>&1; then
  shell=(dash)
else
  shell=(sh)
fi
command -v python3 >/dev/null 2>&1 || {
  echo "test-install.sh: python3 is needed for the local server" >&2
  exit 1
}

tmp=$(mktemp -d)
server=
cleanup() {
  if [ -n "$server" ]; then kill "$server" 2>/dev/null || true; fi
  rm -rf "$tmp"
}
trap cleanup EXIT

# The server: a path's `?` is read as `@` in the file's name; `<file>.status` holds another
# status than 200; a file not there is a 404. Every path asked for is written to the log. It
# stops by itself after ten minutes.
mkdir -p "$tmp/www"
: >"$tmp/requests.log"
python3 -I -c '
import http.server, os, sys, threading
root, port_file, log = sys.argv[1:4]
class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        with open(log, "a") as f:
            f.write(self.path + "\n")
        path = os.path.normpath(os.path.join(root, self.path.lstrip("/").replace("?", "@")))
        status, body = 404, b"{\"message\": \"Not Found\"}"
        if path.startswith(root + os.sep):
            if os.path.isfile(path):
                status = 200
                with open(path, "rb") as f:
                    body = f.read()
            if os.path.isfile(path + ".status"):
                with open(path + ".status") as f:
                    status = int(f.read())
        self.send_response(status)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *args):
        pass
server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
threading.Timer(600, os._exit, [0]).start()
with open(port_file + ".new", "w") as f:
    f.write(str(server.server_address[1]))
os.rename(port_file + ".new", port_file)
server.serve_forever()
' "$tmp/www" "$tmp/port" "$tmp/requests.log" &
server=$!
for _ in $(seq 100); do
  [ -s "$tmp/port" ] && break
  sleep 0.05
done
[ -s "$tmp/port" ] || {
  echo "test-install.sh: the local server did not start" >&2
  exit 1
}
base="http://127.0.0.1:$(cat "$tmp/port")"
download="$base/dl/releases/download"

# --- Fake releases ---------------------------------------------------------------------------

# A fake archive of the version $1, as tools/package.sh lays it out, its `gamme` answering
# --version for the channel $2. With $3 = broken, its `gamme` fails as a binary missing a
# library does; with $3 = wrong-sum, the sum published is not its own.
make_archive() {
  local version=$1 channel=$2 how=${3:-}
  local folder="$tmp/build/gamme-$version" out="$tmp/www/dl/releases/download/v$version"
  local name="gamme-$version-x86_64-linux.tar.gz"
  mkdir -p "$folder/share/applications" "$out"
  if [ "$how" = broken ]; then
    printf '#!/bin/sh\necho "gamme: error while loading shared libraries: libfake.so.1" >&2\nexit 127\n' >"$folder/gamme"
  else
    printf '#!/bin/sh\nif [ "${1:-}" = --version ]; then echo "gamme %s (%s, 0123456789ab)"; fi\n' \
      "$version" "$channel" >"$folder/gamme"
  fi
  chmod 0755 "$folder/gamme"
  cp "$root/LICENSE" "$folder/LICENSE"
  cp "$root/packaging/linux/gamme.desktop" "$folder/share/applications/"
  cp -r "$root/packaging/linux/icons" "$folder/share/icons"
  tar -C "$tmp/build" -czf "$out/$name" "gamme-$version"
  (cd "$out" && sha256sum "$name" >"$name.sha256")
  if [ "$how" = wrong-sum ]; then
    printf '%064d  %s\n' 0 "$name" >"$out/$name.sha256"
  fi
}

# A release of the API, as GitHub writes it: its tag, its dates (published_at null for a draft)
# and its assets, then its notes, holding commas and an install line.
release_json() {
  local version=$1 published=$2 prerelease=false draft=false at
  case "$version" in *-*) prerelease=true ;; esac
  if [ "$published" = null ]; then
    draft=true
    at=null
  else
    at="\"$published\""
  fi
  local name="gamme-$version-x86_64-linux.tar.gz"
  printf '{"url":"%s/api/releases/1","id":1,"author":{"login":"ci","type":"Bot"},"tag_name":"v%s","target_commitish":"dev","name":"Gamme %s","draft":%s,"prerelease":%s,"created_at":"2026-01-01T00:00:00Z","published_at":%s,"assets":[{"name":"%s","state":"uploaded","created_at":"2026-01-01T00:00:00Z","browser_download_url":"%s/v%s/%s"},{"name":"%s.sha256","created_at":"2026-01-01T00:00:00Z","browser_download_url":"%s/v%s/%s.sha256"}],"tarball_url":"%s/tarball/v%s","body":"Gamme %s, built from 0123456789ab.\\n\\nLinux x86_64. Install: `curl -fsSL https://example.invalid/install.sh | sh -s -- --channel beta`, then run it."}' \
    "$base" "$version" "$version" "$draft" "$prerelease" "$at" \
    "$name" "$download" "$version" "$name" "$name" "$download" "$version" "$name" \
    "$base" "$version" "$version"
}

# The API at $1 (a folder of the server): the releases "version published-at" read from stdin,
# newest first as GitHub lists them; `latest` the stable one named by $2, when given.
make_api() {
  local api="$tmp/www/$1" latest=${2:-} version published first=1
  rm -rf "$api"
  mkdir -p "$api/releases/tags"
  {
    printf '['
    while read -r version published; do
      [ -n "$version" ] || continue
      release_json "$version" "$published" >"$api/releases/tags/v$version"
      if [ "$published" = null ]; then rm "$api/releases/tags/v$version"; fi
      if [ -z "$first" ]; then printf ','; fi
      first=
      release_json "$version" "$published"
    done
    printf ']'
  } >"$api/releases@per_page=100"
  if [ -n "$latest" ]; then cp "$api/releases/tags/v$latest" "$api/releases/latest"; fi
}

make_archive 0.1.0 stable
make_archive 0.1.1 stable
make_archive 0.2.0-beta.12 beta
make_archive 0.1.2-beta.3 beta
make_archive 0.2.0-beta.13 beta
make_archive 0.2.0-alpha.feat-a-b.3 alpha
make_archive 0.2.0-alpha.feat-a-b.10 alpha
make_archive 0.2.0-alpha.feat-a-b-c.20 alpha
make_archive 0.2.0-alpha.feat-a.30 alpha
make_archive 0.3.0 stable broken
make_archive 0.4.0 stable wrong-sum

releases() {
  cat <<'EOF'
0.2.0-beta.13 null
0.2.0-alpha.feat-a-b.3 2026-11-10T12:00:00Z
0.1.2-beta.3 2026-11-10T09:00:00Z
0.2.0-alpha.feat-a-b-c.20 2026-11-09T12:00:00Z
0.2.0-alpha.feat-a.30 2026-11-09T11:00:00Z
0.2.0-alpha.feat-a-b.10 2026-11-09T10:00:00Z
0.2.0-beta.12 2026-11-08T09:00:00Z
0.4.0 2026-11-07T09:00:00Z
0.3.0 2026-11-06T09:00:00Z
0.1.1 2026-11-05T09:00:00Z
0.1.0 2026-11-01T09:00:00Z
EOF
}
# Before 0.1.1: 0.1.0 is the latest stable.
grep -v -e '^0\.1\.1 ' -e '^0\.3\.0 ' -e '^0\.4\.0 ' < <(releases) | make_api api 0.1.0
make_api empty </dev/null
mkdir -p "$tmp/www/broken/releases"
echo 500 >"$tmp/www/broken/releases/latest.status"
# A port nothing listens on: the server's, once it is stopped, is not known; 1 is reserved.
nowhere="http://127.0.0.1:1"

# --- Running install.sh ----------------------------------------------------------------------

passed=0
failed=0
ok() {
  passed=$((passed + 1))
  echo "ok - $1"
}
not_ok() {
  failed=$((failed + 1))
  echo "FAIL - $1"
  echo "  exit $status; stdout:"
  sed 's/^/    | /' "$tmp/out"
  echo "  stderr:"
  sed 's/^/    | /' "$tmp/err"
}
check() {
  local what=$1
  shift
  if "$@"; then ok "$what"; else not_ok "$what"; fi
}
said() { grep -qF -- "$1" "$tmp/out" "$tmp/err"; }
exited() { [ "$status" -eq "$1" ]; }

# A new home, with a space in its path; its data folder, and an empty TMPDIR, inside it.
fresh_home() {
  home="$tmp/home $1"
  rm -rf "$home"
  mkdir -p "$home/tmp" "$tmp/cwd"
  bin="$home/.local/bin"
  data="$home/data"
}

# Runs install.sh as `curl … | sh -s -- <args>` does, from a folder that is not the clone, with
# the API $api_root (a folder of the server, or an address with ://).
api_root=api
install_sh() {
  local api=$api_root
  case "$api" in *://*) ;; *) api="$base/$api" ;; esac
  status=0
  (cd "$tmp/cwd" && env HOME="$home" XDG_DATA_HOME="$data" TMPDIR="$home/tmp" \
    GAMME_RELEASES="$api" "${shell[@]}" -s -- "$@" <"$script" >"$tmp/out" 2>"$tmp/err") ||
    status=$?
}
installed() { [ -x "$bin/gamme" ] && [ "$("$bin/gamme" --version)" = "$1" ]; }
nothing_installed() { [ ! -e "$bin/gamme" ] && [ ! -e "$data/applications/gamme.desktop" ]; }
temp_removed() { [ -z "$(ls -A "$home/tmp")" ]; }
requests() { wc -l <"$tmp/requests.log"; }
no_request_since() { [ "$(requests)" -eq "$1" ]; }
exec_line() {
  local real
  real="$(cd "$bin" && pwd -P)/gamme"
  grep -qxF "Exec=\"$real\"" "$data/applications/gamme.desktop"
}
icon() { diff -r "$data/icons/hicolor" "$root/packaging/linux/icons/hicolor" >/dev/null 2>&1; }

echo "# install.sh under ${shell[*]}, against $base"

# Help and wrong uses: exit 2, nothing asked of the server, nothing installed.
fresh_home usage
install_sh --help
check "--help prints the options" eval 'exited 0 && said "--channel alpha --branch"'
before=$(requests)
for args in "--channel" "--branch" "--version" "--version " "--bogus" "--channel nightly" \
  "--channel alpha" "--branch feat/x" "--channel beta --branch feat/x" \
  "--version 0.1.0 --channel beta" "--version 0.1.0 --channel stable" \
  "--version 0.1.0 --branch feat/x" "--version 1.2" "--version 1.2.3/../x" \
  "--version v1.2.3" "--version 01.2.3" "--uninstall --channel beta"; do
  read -ra words <<<"$args"
  if [ "$args" = "--version " ]; then words=(--version ""); fi
  install_sh "${words[@]}"
  check "'$args' is refused (exit 2)" eval 'exited 2 && nothing_installed'
done
check "the alpha channel without --branch says so" eval 'install_sh --channel alpha; said "needs --branch"'
check "--version with --channel says why" eval 'install_sh --version 0.1.0 --channel beta; said "goes without --channel and --branch"'
check "no wrong use reached the server" no_request_since "$before"

# Install, then update.
fresh_home main
install_sh
check "install: the latest stable is installed" eval 'exited 0 && installed "gamme 0.1.0 (stable, 0123456789ab)"'
check "install: --version's answer is printed" said "gamme 0.1.0 (stable, 0123456789ab)"
check "install: the desktop entry runs the real path, quoted" exec_line
check "install: the entry keeps the package's other lines" grep -qx "StartupWMClass=gamme" "$data/applications/gamme.desktop"
check "install: the icon is in the data folder" icon
check "install: ~/.local/bin not in PATH is said" said "Add $bin to your PATH"
check "install: nothing left beside the binary" eval '[ ! -e "$bin/.gamme.new" ] && [ ! -e "$data/applications/.gamme.desktop.new" ]'
check "install: the download folder is removed" temp_removed
check "install: a first install says nothing of an open window" eval '! said "already open"'

releases | make_api api 0.1.1
install_sh
check "update: the new stable replaces the former" eval 'exited 0 && installed "gamme 0.1.1 (stable, 0123456789ab)"'
check "update: an open window keeping the former version is said" said "already open keeps the former version"

# --version.
install_sh --version 0.2.0-beta.12
check "--version installs that release exactly" eval 'exited 0 && installed "gamme 0.2.0-beta.12 (beta, 0123456789ab)"'
install_sh --version 0.2.0-beta.99
check "--version of a release not there: exit 1, said" eval 'exited 1 && said "there is no release 0.2.0-beta.99"'
check "--version of a release not there: close versions given" said "close versions: 0.2.0-alpha.feat-a.30 0.2.0-beta.12 0.2.0-beta.13 0.3.0 0.4.0"
check "--version of a release not there: the installed one is kept" installed "gamme 0.2.0-beta.12 (beta, 0123456789ab)"
check "--version of a release not there: nothing downloaded" eval '! grep -q "gamme-0.2.0-beta.99" "$tmp/requests.log"'

# Beta: the last published, not the highest; never a draft.
install_sh --channel beta
check "beta: the one published last, not the highest nor the draft" eval 'exited 0 && installed "gamme 0.1.2-beta.3 (beta, 0123456789ab)"'

# Alpha: the highest of the branch's slug, whole.
install_sh --channel alpha --branch feat/a_b
check "alpha feat/a_b: the highest of feat-a-b, not feat-a-b-c's" eval 'exited 0 && installed "gamme 0.2.0-alpha.feat-a-b.10 (alpha, 0123456789ab)"'
install_sh --channel alpha --branch feat/a
check "alpha feat/a: feat-a's own, not feat-a-b's" eval 'exited 0 && installed "gamme 0.2.0-alpha.feat-a.30 (alpha, 0123456789ab)"'
install_sh --channel alpha --branch feat/none
check "alpha of a branch with none: exit 1, said" eval 'exited 1 && said "there is no alpha release for feat/none"'
check "alpha of a branch with none: the installed one is kept" installed "gamme 0.2.0-alpha.feat-a.30 (alpha, 0123456789ab)"

# A binary that does not answer stays installed, and that is said.
install_sh --version 0.3.0
check "a binary that does not answer --version stays installed, said" \
  eval 'exited 0 && [ -x "$bin/gamme" ] && said "did not answer" && said "libfake.so.1"'

# From the clone, as ./install.sh.
status=0
(cd "$root" && env HOME="$home" XDG_DATA_HOME="$data" TMPDIR="$home/tmp" \
  GAMME_RELEASES="$base/api" "${shell[@]}" ./install.sh --version 0.1.0 >"$tmp/out" 2>"$tmp/err") ||
  status=$?
check "./install.sh from the clone installs as well" eval 'exited 0 && installed "gamme 0.1.0 (stable, 0123456789ab)"'

# The launcher's files failing is said, not undone.
fresh_home readonly
real_data=$data
data="$home/a file"
: >"$data"
install_sh
check "a data folder that cannot be written: installed all the same, said" \
  eval 'exited 0 && installed "gamme 0.1.1 (stable, 0123456789ab)" && said "the launcher entry was not written"'
data=$real_data

# A wrong sum installs nothing.
fresh_home sum
install_sh --version 0.4.0
check "a wrong sum: exit 1, said" eval 'exited 1 && said "is not the one published: nothing was installed"'
check "a wrong sum: nothing installed" eval 'nothing_installed'
check "a wrong sum: the download folder is removed" temp_removed

# A sum's file that would check another file than the archive: refused before sha256sum runs.
# Its releases are only in the API's tags, so the lists above stay as they are.
# 0.5.0: the right sum of the API's answer, which install.sh keeps beside the archive.
make_archive 0.5.0 stable
release_json 0.5.0 2026-11-07T10:00:00Z >"$tmp/www/api/releases/tags/v0.5.0"
answer_sum=$(sha256sum <"$tmp/www/api/releases/tags/v0.5.0")
printf '%s  answer.json\n' "${answer_sum%% *}" \
  >"$tmp/www/dl/releases/download/v0.5.0/gamme-0.5.0-x86_64-linux.tar.gz.sha256"
# 0.5.1: the archive's own line, twice.
make_archive 0.5.1 stable
release_json 0.5.1 2026-11-07T11:00:00Z >"$tmp/www/api/releases/tags/v0.5.1"
sum_file="$tmp/www/dl/releases/download/v0.5.1/gamme-0.5.1-x86_64-linux.tar.gz.sha256"
cat "$sum_file" "$sum_file" >"$tmp/two-lines"
mv "$tmp/two-lines" "$sum_file"
# 0.5.2: the archive's right sum, written by its path rather than its name.
make_archive 0.5.2 stable
release_json 0.5.2 2026-11-07T12:00:00Z >"$tmp/www/api/releases/tags/v0.5.2"
sum_file="$tmp/www/dl/releases/download/v0.5.2/gamme-0.5.2-x86_64-linux.tar.gz.sha256"
sed 's|  gamme-|  ./gamme-|' "$sum_file" >"$tmp/by-path"
mv "$tmp/by-path" "$sum_file"
for version in 0.5.0 0.5.1 0.5.2; do
  fresh_home "sum-names-$version"
  install_sh --version "$version"
  check "a sum's file not one line naming the archive ($version): exit 1, said, nothing installed" \
    eval 'exited 1 && said "is not one line naming it: nothing was installed" && nothing_installed'
  check "a sum's file not one line naming the archive ($version): the download folder is removed" temp_removed
done

# 404, network and GitHub failures.
fresh_home failures
api_root=empty
install_sh
check "no stable release (404): exit 1, said" eval 'exited 1 && said "there is no stable release yet" && nothing_installed'
install_sh --channel beta
check "no beta release: exit 1, said" eval 'exited 1 && said "there is no beta release"'
install_sh --version 0.1.0
check "--version with no release (404): exit 1, said" eval 'exited 1 && said "there is no release 0.1.0"'
api_root=$nowhere
install_sh
check "the network failing is said as such" eval 'exited 1 && said "is the network up?"'
api_root=broken
install_sh
check "GitHub failing is said as such" eval 'exited 1 && said "GitHub answered 500"'
api_root=api

# --uninstall, without a clone nor the network.
fresh_home uninstall
api_root=api
install_sh
check "an install to remove" eval 'exited 0 && exec_line && icon'
api_root=$nowhere
install_sh --uninstall
check "--uninstall: exit 0" exited 0
check "--uninstall: gamme removed" eval '[ ! -e "$bin/gamme" ]'
check "--uninstall: the entry and the icon removed" \
  eval '[ ! -e "$data/applications/gamme.desktop" ] && [ ! -e "$data/icons/hicolor/scalable/apps/gamme.svg" ] && [ ! -e "$data/icons/hicolor/48x48/apps/gamme.png" ]'
install_sh --uninstall
check "--uninstall again: nothing to remove, exit 0" exited 0
api_root=api

# --- gamme update ------------------------------------------------------------------------------
# The real binary (GAMME_BIN, else the debug or release build), copied into a temporary
# ~/.local/bin, updates itself from the same fake server; the archives hold the fake `gamme`
# scripts above, so what it was replaced by answers --version with the release's.

real_binary=${GAMME_BIN:-}
if [ -z "$real_binary" ]; then
  for candidate in "$root/target/debug/gamme" "$root/target/release/gamme"; do
    if [ -x "$candidate" ]; then real_binary=$candidate; break; fi
  done
fi
if [ -z "$real_binary" ] || [ ! -x "$real_binary" ]; then
  echo "# gamme update: SKIPPED, no binary (cargo build, or GAMME_BIN)"
else
  echo "# gamme update, the real binary $real_binary, against $base"
  # A local build is the crate's version, which the "already" checks below rely on.
  [ "$("$real_binary" --version | cut -d ' ' -f 2)" = 0.1.0 ] || {
    echo "test-install.sh: $real_binary is not a local build at 0.1.0" >&2
    exit 1
  }
  # A release older than the build, for the refusal of a step back (its tag only).
  make_archive 0.0.9 stable
  release_json 0.0.9 2026-10-01T09:00:00Z >"$tmp/www/api/releases/tags/v0.0.9"

  state="" # $XDG_STATE_HOME of the run: kept between runs of one home
  # A new home with the real binary in ~/.local/bin, and its own state folder.
  fresh_binary() {
    fresh_home "$1"
    state="$home/state"
    mkdir -p "$bin" "$state"
    cp "$real_binary" "$bin/gamme"
  }
  update_sh() {
    local api=$api_root
    case "$api" in *://*) ;; *) api="$base/$api" ;; esac
    status=0
    (cd "$tmp/cwd" && env HOME="$home" XDG_DATA_HOME="$data" XDG_STATE_HOME="$state" \
      TMPDIR="$home/tmp" GAMME_RELEASES="$api" "$bin/gamme" update "$@" >"$tmp/out" 2>"$tmp/err") ||
      status=$?
  }
  unchanged() { cmp -s "$bin/gamme" "$real_binary"; }
  nothing_beside() { [ "$(ls -A "$bin")" = gamme ]; }
  chosen() { grep -qF "$1" "$state/gamme/update-channel.json"; }

  # A local build takes stable.
  fresh_binary update-stable
  update_sh
  check "update: a local build takes the latest stable" \
    eval 'exited 0 && installed "gamme 0.1.1 (stable, 0123456789ab)" && said "is now 0.1.1"'
  check "update: the entry runs the real path, quoted" exec_line
  check "update: the icon is in the data folder" icon
  check "update: nothing left beside the binary" nothing_beside
  check "update: no --channel, nothing remembered" eval '[ ! -e "$state/gamme/update-channel.json" ]'

  # The same version is not installed again; the entry follows the binary.
  fresh_binary update-same
  update_sh --version 0.1.0
  check "update: the same version says so and replaces nothing" \
    eval 'exited 0 && unchanged && said "already 0.1.0" && nothing_beside'
  check "update: the entry is not made by an update that installs nothing" \
    eval '[ ! -e "$data/applications/gamme.desktop" ]'
  mkdir -p "$data/applications"
  printf '[Desktop Entry]\nName=Gamme\nExec=/moved/gamme\nIcon=gamme\n' >"$data/applications/gamme.desktop"
  update_sh --version 0.1.0
  check "update: an entry pointing elsewhere is pointed at the binary again" exec_line

  # An older release is refused unless forced.
  update_sh --version 0.0.9
  check "update: an older version is refused, nothing replaced" \
    eval 'exited 1 && unchanged && said "older than this 0.1.0" && said "--force"'
  update_sh --version 0.0.9 --force
  check "update: --force goes back to an older version" \
    eval 'exited 0 && installed "gamme 0.0.9 (stable, 0123456789ab)"'
  fresh_binary update-force
  update_sh --version 0.1.0 --force
  check "update: --force installs the same version again" \
    eval 'exited 0 && installed "gamme 0.1.0 (stable, 0123456789ab)"'

  # Channels, remembered between updates.
  fresh_binary update-beta
  update_sh --channel beta
  check "update: --channel beta takes the beta published last" \
    eval 'exited 0 && installed "gamme 0.1.2-beta.3 (beta, 0123456789ab)"'
  check "update: --channel is remembered" chosen '"channel":"beta"'
  cp "$real_binary" "$bin/gamme"
  update_sh
  check "update: a plain update follows the channel last chosen" \
    eval 'exited 0 && said "Following the channel last chosen: beta" && installed "gamme 0.1.2-beta.3 (beta, 0123456789ab)"'
  cp "$real_binary" "$bin/gamme"
  update_sh --channel alpha --branch feat/a_b
  check "update: an alpha is the highest of its branch's slug" \
    eval 'exited 0 && installed "gamme 0.2.0-alpha.feat-a-b.10 (alpha, 0123456789ab)"'
  check "update: the alpha's branch is remembered" chosen '"branch":"feat/a_b"'
  cp "$real_binary" "$bin/gamme"
  update_sh
  check "update: a plain update follows the alpha and its branch" \
    eval 'exited 0 && said "alpha (feat/a_b)" && installed "gamme 0.2.0-alpha.feat-a-b.10 (alpha, 0123456789ab)"'
  cp "$real_binary" "$bin/gamme"
  update_sh --version 0.1.1
  check "update: --version does not touch the channel remembered" \
    eval 'exited 0 && installed "gamme 0.1.1 (stable, 0123456789ab)" && chosen "\"channel\":\"alpha\""'
  cp "$real_binary" "$bin/gamme"
  update_sh --channel alpha --branch feat/none
  check "update: an alpha of a branch with none: exit 1, said, nothing replaced" \
    eval 'exited 1 && unchanged && said "there is no alpha release for feat/none"'

  # Failures replace nothing and leave nothing.
  fresh_binary update-failures
  update_sh --version 0.4.0
  check "update: a wrong sum replaces nothing" \
    eval 'exited 1 && unchanged && said "is not the one published: nothing was replaced" && nothing_beside'
  update_sh --version 0.0.1
  check "update: a version not there is refused with the nearest ones" \
    eval 'exited 1 && unchanged && said "there is no release 0.0.1" && said "close versions: 0.1.0, 0.1.1, 0.1.2-beta.3"'
  check "update: a version not there downloads nothing" eval '! grep -q "gamme-0.0.1" "$tmp/requests.log"'
  api_root=empty
  update_sh
  check "update: no stable release (404): exit 1, said" \
    eval 'exited 1 && unchanged && said "there is no stable release yet"'
  api_root=$nowhere
  update_sh
  check "update: the network failing is said" eval 'exited 1 && unchanged && said "cannot read"'
  api_root=broken
  update_sh
  check "update: GitHub failing is said" eval 'exited 1 && unchanged && said "GitHub answered 500"'
  api_root=api
  before=$(requests)
  for args in "--channel" "--channel nightly" "--branch feat/x" "--channel beta --branch feat/x" \
    "--version 0.1.0 --channel beta" "--version 1.2" "--version v1.2.3" "--bogus"; do
    read -ra words <<<"$args"
    update_sh "${words[@]}"
    check "update '$args' is refused (exit 2 or 1) and nothing replaced" \
      eval '[ "$status" -ge 1 ] && [ "$status" -le 2 ] && unchanged'
  done
  check "update: no wrong use reached the server" no_request_since "$before"
  update_sh --help
  check "update --help prints its usage" eval 'exited 0 && said "usage: gamme update"'

  # The launcher's files failing is said, not undone.
  fresh_binary update-readonly
  real_data=$data
  data="$home/a file"
  : >"$data"
  update_sh
  check "update: a data folder that cannot be written: replaced all the same, said" \
    eval 'exited 0 && installed "gamme 0.1.1 (stable, 0123456789ab)" && said "the launcher entry was not updated"'
  data=$real_data
fi

echo "# $passed passed, $failed failed (install.sh under ${shell[*]})"
[ "$failed" -eq 0 ]
