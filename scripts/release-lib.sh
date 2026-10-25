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

# previous_feed_version HTTP_CODE FILE: the version published in the fetched feed.
# A 404 means no feed yet (prints nothing, succeeds); a 200 must carry a
# sparkle:version; any other answer fails.
previous_feed_version() {
  local code="$1" file="$2" version
  case "$code" in
    404) return 0 ;;
    200)
      version="$(appcast_version "$file")"
      if [ -z "$version" ]; then
        echo "::error::published appcast has no sparkle:version" >&2
        return 1
      fi
      echo "$version"
      ;;
    *)
      echo "::error::fetching the published appcast returned HTTP $code" >&2
      return 1
      ;;
  esac
}

# formula_version FILE: the version in a rendered btp formula's release url.
formula_version() {
  sed -n 's|.*/releases/download/v\([0-9][^/]*\)/.*|\1|p' "$1" | head -1
}

# tap_is_ahead TAP_VERSION VERSION: succeeds when the tap already holds a
# version above VERSION (sort -V), so publishing VERSION would downgrade it.
# An empty TAP_VERSION (no formula yet) is never ahead.
tap_is_ahead() {
  local tap="${1:-}" version="$2"
  [ -n "$tap" ] || return 1
  [ "$tap" != "$version" ] || return 1
  [ "$(printf '%s\n%s\n' "$tap" "$version" | sort -V | tail -1)" = "$tap" ]
}
