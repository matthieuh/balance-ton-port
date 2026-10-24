#!/bin/bash
# Tests for the release helper scripts (render-homebrew.sh and the Homebrew
# templates). Fixtures only, never touches the network or the real tap.
#
#   scripts/test-release-scripts.sh

# The bash -c snippets take their values as arguments, single quotes on purpose.
# shellcheck disable=SC2016

set -uo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
render="$repo/scripts/render-homebrew.sh"
work="$(mktemp -d "${TMPDIR:-/tmp}/btp-release-tests.XXXXXX")"
trap 'rm -rf "$work"' EXIT

pass=0
fail=0
ok() {
  pass=$((pass + 1))
  printf 'ok   %s\n' "$1"
}
ko() {
  fail=$((fail + 1))
  printf 'FAIL %s\n' "$1"
  [ $# -lt 2 ] || printf '     %s\n' "$2"
}
check() { # check NAME CMD...
  local name="$1"
  shift
  if "$@"; then ok "$name"; else ko "$name"; fi
}

SHA_MACOS="$(printf 'a%.0s' {1..64})"
SHA_DMG="$(printf 'd%.0s' {1..64})"

# --- render: every token replaced -------------------------------------------
out="$work/ok"
"$render" 1.2.3 "$SHA_MACOS" "$SHA_DMG" "$out" >/dev/null 2>&1

tokens_replaced() {
  [ -f "$out/btp.rb" ] && [ -f "$out/balance-ton-port.rb" ] || return 1
  [ "$(grep -c "$SHA_MACOS" "$out/btp.rb")" -eq 1 ] || return 1
  [ "$(grep -c "$SHA_DMG" "$out/balance-ton-port.rb")" -eq 1 ] || return 1
  ! grep -q "$SHA_DMG" "$out/btp.rb" || return 1
  ! grep -q "$SHA_MACOS" "$out/balance-ton-port.rb" || return 1
  grep -q 'releases/download/v1.2.3/btp-1.2.3-macos-universal.tar.gz' "$out/btp.rb" || return 1
  grep -q '^  version "1.2.3"$' "$out/balance-ton-port.rb" || return 1
  ! grep -q '@[A-Z_0-9]*@' "$out/btp.rb" "$out/balance-ton-port.rb"
}
check "render: every token replaced" tokens_replaced

# --- templates --------------------------------------------------------------
check "formula is macOS only" bash -c \
  'grep -q "depends_on macos: \">= :sonoma\"" "$1" && ! grep -qE "on_linux|on_intel|on_arm" "$1"' _ "$repo/packaging/homebrew/btp.rb"
check "cask_declares_auto_updates" grep -q '^  auto_updates true$' "$out/balance-ton-port.rb"
check "descs say coding-agent sessions" bash -c \
  'grep -q "coding-agent sessions" "$1" && grep -q "coding-agent sessions" "$2"' _ "$out/btp.rb" "$out/balance-ton-port.rb"

# --- render: validation -----------------------------------------------------
rejects() { # rejects ARGS...
  ! "$render" "$@" "$work/bad" >/dev/null 2>&1
}
check "render: rejects a bad sha" rejects 1.2.3 "not-a-sha" "$SHA_DMG"
check "render: rejects a bad version" rejects "v1" "$SHA_MACOS" "$SHA_DMG"

# --- brew style -------------------------------------------------------------
if command -v brew >/dev/null 2>&1; then
  # Outside a tap, brew style demands the file header a tap's own config
  # exempts (Sorbet sigil, frozen string literal): add it to copies.
  styled="$work/styled"
  mkdir -p "$styled"
  for f in btp balance-ton-port; do
    { printf '# typed: strict\n# frozen_string_literal: true\n\n'; cat "$out/$f.rb"; } >"$styled/$f.rb"
  done
  check "render: brew style passes" brew style "$styled/btp.rb" "$styled/balance-ton-port.rb"
else
  printf 'skip render: brew style passes (brew not installed)\n'
fi

# --- release-lib.sh ------------------------------------------------------
lib="$repo/scripts/release-lib.sh"
for tag in v0.4.0-beta.1 v1.0.0-rc.2; do
  check "is_prerelease: $tag is a prerelease" bash -c '. "$1" && is_prerelease "$2"' _ "$lib" "$tag"
done
for tag in v0.4.0 v10.2.3; do
  if bash -c '. "$1" && is_prerelease "$2"' _ "$lib" "$tag" 2>/dev/null; then
    ko "is_prerelease: $tag is final"
  else
    ok "is_prerelease: $tag is final"
  fi
done

# --- release-lib.sh: appcast helpers ---------------------------------------
increases() { bash -c '. "$1" && bundle_version_increases "$2" "$3"' _ "$lib" "$@" 2>/dev/null; }
check "bundle_version_must_increase: 8 over 7" increases 8 7
check "bundle_version_must_increase: 10 over 9 (numeric, not text)" increases 10 9
check "bundle_version_must_increase: first release (no previous)" increases 8 ""
for pair in "7 7" "6 7" "x 7" "8 x" "0 0" ""; do
  # shellcheck disable=SC2086
  if increases ${pair:-"" ""}; then
    ko "bundle_version_must_increase: rejects '$pair'"
  else
    ok "bundle_version_must_increase: rejects '$pair'"
  fi
done

# --- render-appcast.sh -----------------------------------------------------
appcast="$repo/scripts/render-appcast.sh"
SIG="$(printf 'QUJD%.0s' {1..21})AA=="
mkdir -p "$work/appcast"
"$appcast" 1.2.3 8 BalanceTonPort-1.2.3.dmg 4567890 "$SIG" "Fri, 23 Oct 2026 10:00:00 +0000" "$work/appcast/appcast.xml" >/dev/null 2>&1
check "appcast: matches the fixture" diff "$repo/scripts/fixtures/appcast-expected.xml" "$work/appcast/appcast.xml"
appcast_rejects() { ! "$appcast" "$@" "Fri, 23 Oct 2026 10:00:00 +0000" "$work/appcast/bad.xml" >/dev/null 2>&1; }
check "appcast: rejects a non-integer length" appcast_rejects 1.2.3 8 BalanceTonPort-1.2.3.dmg 12abc "$SIG"
check "appcast: rejects a bad signature" appcast_rejects 1.2.3 8 BalanceTonPort-1.2.3.dmg 100 "not base64!"
check "appcast: rejects a bad bundle version" appcast_rejects 1.2.3 1.2 BalanceTonPort-1.2.3.dmg 100 "$SIG"
check "appcast: rejects a bad short version" appcast_rejects v1 8 BalanceTonPort-1.2.3.dmg 100 "$SIG"
check "appcast: rejects a dmg name with a slash" appcast_rejects 1.2.3 8 ../x.dmg 100 "$SIG"
check "appcast_version reads the first item" bash -c \
  '. "$1" && [ "$(appcast_version "$2")" = "8" ]' _ "$lib" "$work/appcast/appcast.xml"
two="$work/appcast/two.xml"
sed 's|</channel>|<item><sparkle:version>99</sparkle:version></item></channel>|' "$work/appcast/appcast.xml" >"$two"
check "appcast_version ignores later items" bash -c \
  '. "$1" && [ "$(appcast_version "$2")" = "8" ]' _ "$lib" "$two"
check "appcast: fixture is well-formed XML" xmllint --noout "$repo/scripts/fixtures/appcast-expected.xml"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
