#!/usr/bin/env bash
# Renders the Homebrew templates with dummy values into a throwaway local tap
# and runs Homebrew's own checks on them (audit, install dry run), so a
# formula or cask Homebrew refuses to load fails CI, not the release.
#
#   scripts/check-homebrew.sh
#
# Needs brew. Trust entries go to a temp XDG_CONFIG_HOME, and the tap is
# untapped on exit.
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
work="$(mktemp -d)"
tap="btpci/local"
export XDG_CONFIG_HOME="$work/config" HOMEBREW_NO_AUTO_UPDATE=1 HOMEBREW_NO_ENV_HINTS=1
trap 'brew untap "$tap" >/dev/null 2>&1 || true; rm -rf "$work"' EXIT

sha="$(printf '0%.0s' {1..64})"
"$repo/scripts/render-homebrew.sh" 0.0.1 "$sha" "$sha" "$work/out" >/dev/null

brew tap-new --no-git "$tap" >/dev/null
dir="$(brew --repo "$tap")"
mkdir -p "$dir/Formula" "$dir/Casks"
cp "$work/out/btp.rb" "$dir/Formula/btp.rb"
cp "$work/out/balance-ton-port.rb" "$dir/Casks/balance-ton-port.rb"
brew trust "$tap" >/dev/null

# Deprecation warnings would be the next release's errors.
check() { # check DESCRIPTION CMD...
    local log="$work/log"
    if ! "${@:2}" >"$log" 2>&1 || grep -qi "deprecated" "$log"; then
        echo "FAIL $1" >&2
        cat "$log" >&2
        exit 1
    fi
    echo "ok   $1"
}
check "formula audit" brew audit --formula "$tap/btp"
check "cask audit" brew audit --cask "$tap/balance-ton-port"
check "formula install dry run" brew install --dry-run "$tap/btp"
check "cask install dry run" brew install --cask --dry-run "$tap/balance-ton-port"
