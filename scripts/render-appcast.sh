#!/usr/bin/env bash
# Writes the Sparkle appcast of a final release: one item, the signed dmg.
#
#   scripts/render-appcast.sh <short version> <bundle version> <dmg name> <dmg length> <ed signature> <pub date RFC 822> <out file>
#
# The release workflow runs it after sign_update and attaches the result to the
# GitHub release as appcast.xml, which SUFeedURL (releases/latest/download) serves.
set -euo pipefail

if [ "$#" -ne 7 ]; then
    echo "usage: $0 <short-version> <bundle-version> <dmg-name> <dmg-length> <ed-signature> <pub-date> <out-file>" >&2
    exit 2
fi
SHORT="$1"
BUNDLE="$2"
DMG="$3"
LENGTH="$4"
SIGNATURE="$5"
PUB_DATE="$6"
OUT="$7"

if ! [[ "$SHORT" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "error: bad version '$SHORT'" >&2
    exit 1
fi
if ! [[ "$BUNDLE" =~ ^[1-9][0-9]*$ ]]; then
    echo "error: bad bundle version '$BUNDLE'" >&2
    exit 1
fi
if ! [[ "$DMG" =~ ^[A-Za-z0-9._-]+\.dmg$ ]]; then
    echo "error: bad dmg name '$DMG'" >&2
    exit 1
fi
if ! [[ "$LENGTH" =~ ^[1-9][0-9]*$ ]]; then
    echo "error: bad length '$LENGTH'" >&2
    exit 1
fi
if ! [[ "$SIGNATURE" =~ ^[A-Za-z0-9+/]+={0,2}$ ]]; then
    echo "error: bad signature" >&2
    exit 1
fi
if ! [[ "$PUB_DATE" =~ ^[A-Za-z0-9,:+\ -]+$ ]]; then
    echo "error: bad date '$PUB_DATE'" >&2
    exit 1
fi

mkdir -p "$(dirname "$OUT")"
cat >"$OUT" <<XML
<?xml version="1.0" encoding="utf-8"?>
<rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle">
  <channel>
    <title>Balance ton port</title>
    <item>
      <title>Version $SHORT</title>
      <pubDate>$PUB_DATE</pubDate>
      <sparkle:version>$BUNDLE</sparkle:version>
      <sparkle:shortVersionString>$SHORT</sparkle:shortVersionString>
      <sparkle:minimumSystemVersion>14.0</sparkle:minimumSystemVersion>
      <enclosure url="https://github.com/matthieuh/balance-ton-port/releases/download/v$SHORT/$DMG" length="$LENGTH" type="application/octet-stream" sparkle:edSignature="$SIGNATURE"/>
    </item>
  </channel>
</rss>
XML
echo "$OUT"
