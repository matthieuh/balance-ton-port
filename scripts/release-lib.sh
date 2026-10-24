#!/bin/bash
# Helpers sourced by .github/workflows/release.yml and the release tests.

# is_prerelease TAG: succeeds when the tag has a hyphen (v0.2.0-beta.1).
is_prerelease() {
  [[ "$1" == *-* ]]
}
