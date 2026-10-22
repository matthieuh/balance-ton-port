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
ZERO="0000000000000000000000000000000000000000000000000000000000000000"
mkdir -p "$OUT"

# Formula: the version only appears in the url (v0.0.0 and btp-0.0.0-...).
sed -e "s/0\.0\.0/$VERSION/g" -e "s/$ZERO/$BTP_SHA/" \
    "$SRC/btp.rb" >"$OUT/btp.rb"
# Cask: the url interpolates #{version}.
sed -e "s/^  version \"0\.0\.0\"$/  version \"$VERSION\"/" -e "s/$ZERO/$APP_SHA/" \
    "$SRC/balance-ton-port.rb" >"$OUT/balance-ton-port.rb"

for f in "$OUT/btp.rb" "$OUT/balance-ton-port.rb"; do
    if grep -q -e "$ZERO" -e '0\.0\.0' "$f"; then
        echo "error: placeholder left in $f" >&2
        exit 1
    fi
done
echo "$OUT/btp.rb"
echo "$OUT/balance-ton-port.rb"
