#!/bin/bash
# Installs (or updates) the btp CLI for the Balance ton port plugin.
#
#   install-btp.sh [--data-dir DIR]
#
# Downloads the latest GitHub release of btp, checks its sha256 and its
# Developer ID signature (team 47P2KJ2HF3), and installs it to
# ~/.local/bin/btp. Refuses on any mismatch. Never uses sudo. Replaces only a
# btp this script installed (a marker next to it holds its sha256).
# --data-dir is the plugin's data dir: running this script clears the
# user's earlier "no" to the install that is stored there.

set -euo pipefail

REPO="matthieuh/balance-ton-port"
TEAM_ID="47P2KJ2HF3"

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=SCRIPTDIR/lib.sh
. "$here/lib.sh"

say() { printf 'btp install: %s\n' "$*"; }
refuse() {
  printf 'btp install: refused: %s\n' "$*" >&2
  exit 1
}

data_dir=""
while [ $# -gt 0 ]; do
  case "$1" in
    --data-dir)
      [ $# -ge 2 ] || refuse "--data-dir needs a directory"
      data_dir="$2"
      shift 2
      ;;
    *) refuse "unknown argument: $1" ;;
  esac
done

if [ "$(id -u)" = 0 ]; then
  refuse "do not run this as root; it installs into your own ~/.local/bin"
fi

# The user asked for the install: forget an earlier "no".
if [ -n "$data_dir" ]; then
  rm -f "$data_dir/install-declined"
fi

dest_dir="$HOME/.local/bin"
dest="$dest_dir/btp"
marker="$dest_dir/$BTP_MARKER_NAME"

sha256_of() { shasum -a 256 "$1" | cut -d' ' -f1; }

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
trap 'rm -rf "$tmp"' EXIT

fetch() {
  curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 \
    --retry 2 --max-time 120 -o "$2" "$1"
}

say "looking up the latest release of $REPO"
fetch "https://api.github.com/repos/$REPO/releases/latest" "$tmp/latest.json" ||
  refuse "could not read the latest release from the GitHub API"
tag="$(plutil -extract tag_name raw -o - "$tmp/latest.json" 2>/dev/null)" ||
  refuse "the GitHub API answer has no tag_name"
[[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || refuse "unexpected release tag: $tag"
version="${tag#v}"
if ! btp_version_ge "$version" "$BTP_MIN_VERSION"; then
  refuse "the latest release ($version) is older than this plugin needs ($BTP_MIN_VERSION)"
fi

name="btp-$version-macos-universal.tar.gz"
base="https://github.com/$REPO/releases/download/$tag"
say "downloading $name"
fetch "$base/$name" "$tmp/$name" || refuse "could not download $name"
fetch "$base/$name.sha256" "$tmp/$name.sha256" || refuse "could not download $name.sha256"

read -r want_sha want_name <"$tmp/$name.sha256" || refuse "empty $name.sha256"
[ "${want_name#\*}" = "$name" ] || refuse "$name.sha256 is for another file (${want_name:-none})"
[[ "$want_sha" =~ ^[0-9a-f]{64}$ ]] || refuse "$name.sha256 holds no sha256"
got_sha="$(sha256_of "$tmp/$name")"
[ "$got_sha" = "$want_sha" ] || refuse "sha256 mismatch for $name (expected $want_sha, got $got_sha)"

mkdir "$tmp/x"
tar -xzf "$tmp/$name" -C "$tmp/x" btp || refuse "$name has no btp inside"
new="$tmp/x/btp"
[ -f "$new" ] && [ ! -L "$new" ] || refuse "btp in $name is not a regular file"

codesign --verify --strict "$new" || refuse "the code signature of btp does not verify"
team="$(codesign -d --verbose=2 "$new" 2>&1 | sed -n 's/^TeamIdentifier=//p')"
[ "$team" = "$TEAM_ID" ] || refuse "btp is signed by team ${team:-none}, expected $TEAM_ID"

reported="$(btp_version "$new")" || refuse "the downloaded btp does not run"
[ "$reported" = "$version" ] || refuse "the downloaded btp reports $reported, the release is $version"
"$new" stale --help >/dev/null 2>&1 ||
  refuse "btp $version does not have the session commands this plugin needs yet"

mkdir -p "$dest_dir"
chmod 755 "$new"
cp "$new" "$dest_dir/.btp.new.$$"
mv -f "$dest_dir/.btp.new.$$" "$dest"
sha256_of "$dest" >"$marker"
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
