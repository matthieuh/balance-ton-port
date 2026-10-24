#!/bin/bash
# Installs (or updates) the btp CLI for the Balance ton port plugin.
#
#   install-btp.sh [--data-dir DIR] [--clear-decline]
#
# With Homebrew on PATH it installs (or upgrades) matthieuh/tap/btp instead,
# and removes the copy this script put in ~/.local/bin earlier. BTP_NO_BREW=1
# skips Homebrew. Otherwise, or when brew fails, it downloads the latest
# GitHub release of btp, checks its sha256 and its Developer ID signature
# (Apple's chain, team 47P2KJ2HF3), and installs it to ~/.local/bin/btp.
# Refuses on any mismatch. Never uses sudo. Replaces
# only a btp this script installed (a marker next to it holds its sha256).
#
# --data-dir is the plugin's data dir. A release refused as too old is
# recorded there (install-refused, with its tag) so the SessionStart hook
# stays quiet until a newer one is out; a successful install clears that
# and the user's earlier "no". --clear-decline (the install command) clears
# the "no" up front.

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=SCRIPTDIR/lib.sh
. "$here/lib.sh"

# A signature from Apple's Developer ID chain (Developer ID CA intermediate,
# Developer ID Application leaf) for the project's team. A self-made
# certificate with that team in its subject does not match.
REQUIREMENT='anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = "47P2KJ2HF3"'

say() { printf 'btp install: %s\n' "$*"; }
refuse() {
  printf 'btp install: refused: %s\n' "$*" >&2
  exit 1
}

data_dir=""
clear_decline=""
while [ $# -gt 0 ]; do
  case "$1" in
    --data-dir)
      [ $# -ge 2 ] || refuse "--data-dir needs a directory"
      data_dir="$2"
      shift 2
      ;;
    --clear-decline)
      clear_decline=1
      shift
      ;;
    *) refuse "unknown argument: $1" ;;
  esac
done

# An empty or unsubstituted value (an older Claude Code that does not fill
# in ${CLAUDE_PLUGIN_DATA} in commands): every data dir Claude Code may
# have given this plugin, plus the hooks' own fallback.
data_dirs=()
# shellcheck disable=SC2016 # a literal "${", not an expansion
if [ -n "$data_dir" ] && [[ "$data_dir" != *'${'* ]]; then
  data_dirs=("$data_dir")
else
  for d in "${CLAUDE_CONFIG_DIR:-}" "$HOME"/.claude "$HOME"/.claude-*; do
    [ -n "$d" ] || continue
    for p in "$d"/plugins/data/balance-ton-port*; do
      [ -d "$p" ] && data_dirs+=("$p")
    done
  done
  data_dirs+=("$(CLAUDE_PLUGIN_DATA="" btp_data_dir)")
fi
forget() { # forget FILE...: removes those files from every data dir
  local d f
  for d in "${data_dirs[@]}"; do
    for f in "$@"; do rm -f "$d/$f"; done
  done
}
# refuse_release TAG MESSAGE: refuses and remembers the tag.
refuse_release() {
  local d
  for d in "${data_dirs[@]}"; do
    mkdir -p "$d" && printf '%s\n' "$1" >"$d/install-refused" || true
  done
  refuse "$2"
}

if [ "$(id -u)" = 0 ]; then
  refuse "do not run this as root; it installs into your own ~/.local/bin"
fi

[ -z "$clear_decline" ] || forget install-declined

dest_dir="$HOME/.local/bin"
dest="$dest_dir/btp"
marker="$dest_dir/$BTP_MARKER_NAME"

sha256_of() { shasum -a 256 "$1" | cut -d' ' -f1; }

FORMULA="matthieuh/tap/btp"
use_brew=""
if [ -z "${BTP_NO_BREW:-}" ] && command -v brew >/dev/null 2>&1; then
  use_brew=1
fi
brew_owns_btp() { brew list --formula "$FORMULA" >/dev/null 2>&1; }

if [ -n "$use_brew" ]; then
  if brew_owns_btp; then verb=upgrade; else verb=install; fi
  brew_err="$(mktemp "${TMPDIR:-/tmp}/btp-brew.XXXXXX")"
  say "brew $verb $FORMULA"
  if brew "$verb" "$FORMULA" 2>"$brew_err"; then
    rm -f "$brew_err"
    # The copy this script installed is superseded; one it did not install stays.
    if [ -f "$dest" ] && [ ! -L "$dest" ] && [ -f "$marker" ] && [ "$(cat "$marker")" = "$(sha256_of "$dest")" ]; then
      rm -f "$dest" "$marker"
      say "removed the copy this plugin installed in ~/.local/bin; Homebrew's btp is used now"
    fi
    forget install-declined install-refused
    say "installed btp with Homebrew ($FORMULA)"
    first="$(command -v btp 2>/dev/null || true)"
    if [ -n "$first" ] && [ "$first" = "$dest" ]; then
      say "note: another btp comes first on your PATH: $first"
    fi
    exit 0
  fi
  say "Homebrew could not install it ($(grep -v '^[[:space:]]*$' "$brew_err" | tail -n 1)); using the signed download instead"
  rm -f "$brew_err"
  if brew_owns_btp; then
    refuse "btp is installed with Homebrew; update it with brew upgrade $FORMULA"
  fi
fi

# Only a btp this script installed, unchanged since, may be replaced.
if [ -L "$dest" ]; then
  refuse "$dest is a symbolic link this plugin did not create; remove it or update that btp yourself"
fi
if [ -e "$dest" ]; then
  if [ ! -f "$marker" ] || [ "$(cat "$marker")" != "$(sha256_of "$dest")" ]; then
    refuse "$dest was not installed by this plugin (or changed since); update it yourself, or remove it and run this again"
  fi
fi

tmp="$(mktemp -d "${TMPDIR:-/tmp}/btp-install.XXXXXX")"
staged=""
cleanup() {
  rm -rf "$tmp"
  [ -z "$staged" ] || rm -f "$staged"
}
trap cleanup EXIT

say "looking up the latest release of $BTP_REPO"
btp_fetch "$BTP_LATEST_URL" "$tmp/latest.json" ||
  refuse "could not read the latest release from the GitHub API"
tag="$(btp_tag_of "$tmp/latest.json")" || refuse "the GitHub API answer has no usable tag_name"
version="${tag#v}"
if ! btp_version_ge "$version" "$BTP_MIN"; then
  refuse_release "$tag" "the latest release ($version) is older than this plugin needs ($BTP_MIN); it will offer the install again once a newer release is out"
fi

name="btp-$version-macos-universal.tar.gz"
base="https://github.com/$BTP_REPO/releases/download/$tag"
say "downloading $name"
btp_fetch "$base/$name" "$tmp/$name" || refuse "could not download $name"
btp_fetch "$base/$name.sha256" "$tmp/$name.sha256" || refuse "could not download $name.sha256"

want_sha="" want_name=""
read -r want_sha want_name <"$tmp/$name.sha256" || [ -n "$want_sha" ] || refuse "empty $name.sha256"
[ "${want_name#\*}" = "$name" ] || refuse "$name.sha256 is for another file (${want_name:-none})"
[[ "$want_sha" =~ ^[0-9a-f]{64}$ ]] || refuse "$name.sha256 holds no sha256"
got_sha="$(sha256_of "$tmp/$name")"
[ "$got_sha" = "$want_sha" ] || refuse "sha256 mismatch for $name (expected $want_sha, got $got_sha)"

mkdir "$tmp/x"
tar -xzf "$tmp/$name" -C "$tmp/x" btp || refuse "$name has no btp inside"
new="$tmp/x/btp"
[ -f "$new" ] && [ ! -L "$new" ] || refuse "btp in $name is not a regular file"

codesign --verify --strict "-R=$REQUIREMENT" "$new" ||
  refuse "btp is not signed with the project's Developer ID (team 47P2KJ2HF3)"

reported="$(btp_version "$new")" || refuse "the downloaded btp does not run"
[ "$reported" = "$version" ] || refuse "the downloaded btp reports $reported, the release is $version"
"$new" stale --help >/dev/null 2>&1 ||
  refuse_release "$tag" "btp $version does not have the session commands this plugin needs yet; it will offer the install again once a newer release is out"

mkdir -p "$dest_dir"
staged="$dest_dir/.btp.new.$$"
cp "$new" "$staged"
chmod 755 "$staged"
mv -f "$staged" "$dest"
staged=""
sha256_of "$dest" >"$marker"
forget install-declined install-refused
say "installed btp $version to $dest"

case ":$PATH:" in
  *":$dest_dir:"*) ;;
  *)
    say "$dest_dir is not on your PATH (the plugin's hooks find btp anyway). To use btp in a shell, add this to ~/.zshrc:"
    say "  export PATH=\"\$HOME/.local/bin:\$PATH\""
    ;;
esac
first="$(command -v btp 2>/dev/null || true)"
if [ -n "$first" ] && [ "$first" != "$dest" ]; then
  say "note: another btp comes first on your PATH: $first"
fi
