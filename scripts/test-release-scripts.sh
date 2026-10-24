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

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
