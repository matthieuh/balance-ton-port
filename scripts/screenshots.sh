#!/usr/bin/env bash
# Renders the README screenshots into docs/screenshots/: the app's real
# views fed made-up servers (app/Screenshots/Demo.swift), so nothing from
# this machine shows. Rerun after any change to the menu or the detail.
#
# Requires what the app build needs (see build-xcframework.sh) and xcodegen.
# Run from anywhere.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
out="$root/docs/screenshots"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

"$root/scripts/build-xcframework.sh"
(cd "$root/app" && xcodegen generate --quiet)

# The core the preview model opens gets an empty data dir and home: the
# render never reads or writes the real ones.
mkdir -p "$out" "$tmp/data" "$tmp/home"
TEST_RUNNER_BTP_SCREENSHOTS_DIR="$out" \
TEST_RUNNER_BTP_DATA_DIR="$tmp/data" \
TEST_RUNNER_HOME="$tmp/home" \
  xcodebuild -project "$root/app/BalanceTonPort.xcodeproj" -scheme Screenshots \
    -destination 'platform=macOS' -derivedDataPath "$tmp/dd" test -quiet

ls "$out"
