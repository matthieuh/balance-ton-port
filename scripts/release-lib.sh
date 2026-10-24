#!/bin/bash
# Helpers sourced by .github/workflows/release.yml and the release tests.

# is_prerelease TAG: succeeds when the tag has a hyphen (v0.2.0-beta.1).
is_prerelease() {
  [[ "$1" == *-* ]]
}

# bundle_version_increases NEW PREVIOUS: succeeds when NEW is a positive integer
# above PREVIOUS. An empty PREVIOUS is the first Sparkle release.
bundle_version_increases() {
  local new="$1" previous="${2:-}"
  [[ "$new" =~ ^[1-9][0-9]*$ ]] || return 1
  [ -n "$previous" ] || return 0
  [[ "$previous" =~ ^[1-9][0-9]*$ ]] || return 1
  [ "$new" -gt "$previous" ]
}

# appcast_version FILE: the sparkle:version of the first item of an appcast.
appcast_version() {
  sed -n 's|.*<sparkle:version>\([0-9]*\)</sparkle:version>.*|\1|p' "$1" | head -1
}
