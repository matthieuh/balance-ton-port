#!/usr/bin/env bash
# Fills the Homebrew templates in packaging/homebrew/ with a release's version
# and artifact checksums (btp tarball, app .dmg), ready to copy into matthieuh/homebrew-tap.
#
#   scripts/render-homebrew.sh <version> <btp tarball sha256> <app dmg sha256> <out dir>
#
# Writes <out dir>/btp.rb (formula) and <out dir>/balance-ton-port.rb (cask).
# The release workflow runs it and attaches both files to the GitHub release.
set -euo pipefail

if [ "$#" -ne 4 ]; then
    echo "usage: $0 <version> <btp-sha256> <app-dmg-sha256> <out-dir>" >&2
    exit 2
fi
VERSION="$1"
BTP_SHA="$2"
APP_SHA="$3"
OUT="$4"

if ! [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]]; then
    echo "error: bad version '$VERSION'" >&2
    exit 1
fi
for sha in "$BTP_SHA" "$APP_SHA"; do
    if ! [[ "$sha" =~ ^[0-9a-f]{64}$ ]]; then
        echo "error: bad sha256 '$sha'" >&2
        exit 1
    fi
done

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="$ROOT/packaging/homebrew"
mkdir -p "$OUT"

render() { # render TEMPLATE OUTPUT SED-ARGS...
    local src="$1" dst="$2"
    shift 2
    sed "$@" "$src" >"$dst"
    if grep -q '@[A-Z_0-9]*@' "$dst"; then
        echo "error: token left in $dst" >&2
        exit 1
    fi
}

render "$SRC/btp.rb" "$OUT/btp.rb" -e "s/@VERSION@/$VERSION/g" -e "s/@SHA_MACOS@/$BTP_SHA/g"
render "$SRC/balance-ton-port.rb" "$OUT/balance-ton-port.rb" -e "s/@VERSION@/$VERSION/g" -e "s/@SHA_DMG@/$APP_SHA/g"
echo "$OUT/btp.rb"
echo "$OUT/balance-ton-port.rb"
