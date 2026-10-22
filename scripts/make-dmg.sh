#!/usr/bin/env bash
# Builds the drag-to-install disk image for the menu bar app, with hdiutil only.
#
#   scripts/make-dmg.sh <app path> <version> <out dir>
#
# Writes <out dir>/BalanceTonPort-<version>.dmg: a compressed, read-only (UDZO)
# image whose volume "Balance ton port" holds the app and an Applications link.
# Signing, notarizing and stapling the image are left to the caller.
set -euo pipefail

if [ "$#" -ne 3 ]; then
    echo "usage: $0 <app path> <version> <out dir>" >&2
    exit 2
fi
APP="${1%/}"
VERSION="$2"
OUT="$3"
VOLNAME="Balance ton port"

if ! [ -d "$APP" ] || [ "${APP##*.}" != "app" ]; then
    echo "error: '$APP' is not an .app bundle" >&2
    exit 1
fi
if ! [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]]; then
    echo "error: bad version '$VERSION'" >&2
    exit 1
fi

mkdir -p "$OUT"
DMG="$(cd "$OUT" && pwd)/BalanceTonPort-$VERSION.dmg"

STAGE="$(mktemp -d "${TMPDIR:-/tmp}/btp-dmg.XXXXXX")"
trap 'rm -rf "$STAGE"' EXIT

# ditto keeps the bundle's signature, extended attributes and symlinks intact.
ditto "$APP" "$STAGE/$(basename "$APP")"
ln -s /Applications "$STAGE/Applications"

rm -f "$DMG"
hdiutil create -quiet -volname "$VOLNAME" -srcfolder "$STAGE" -fs HFS+ \
    -format UDZO -imagekey zlib-level=9 -ov "$DMG"
echo "$DMG"
