#!/bin/bash
# Balance ton port, SessionStart hook. Adds context for Claude, never acts:
# - btp missing or too old (and the user did not decline): ask the user
#   whether to install it, run the install script only on yes;
# - btp present: servers left by an earlier conversation of this same
#   Claude Code process (after /clear or /resume) are offered for stopping,
#   in one question.
# Always exits 0. Claude Code passes stdout's additionalContext to Claude.

exec 2>/dev/null
payload="$(cat)"
exec </dev/null

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" || exit 0
# shellcheck source=SCRIPTDIR/../scripts/lib.sh
. "$here/../scripts/lib.sh" || exit 0

emit() {
  printf '{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"%s"}}\n' \
    "$(btp_json_escape "$1")"
}

source="$(btp_json_field source "$payload")"
session_id="$(btp_json_field session_id "$payload")"

if bin="$(btp_find)"; then
  [ -n "$session_id" ] || exit 0
  # Exits 1 when not run from a session: nothing to offer then.
  stale="$("$bin" stale --conversation "$session_id")" || exit 0
  case "$stale" in
    *'"port"'*) ;;
    *) exit 0 ;;
  esac
  emit "Balance ton port (plugin): dev servers started by an earlier conversation of this Claude Code session are still running and have been idle more than 5 minutes. They are listed once, now; btp will not offer them again.

$stale

Before anything else, ask the user ONE question that names each of them (tool label, port, and the agent's note when there is one, for example \"the Vite you started to test the form\"): should they be stopped? On yes, run \`$bin kill <port>\` for each port listed. On no, do nothing. Do not stop them without that yes, and do not ask again."
  exit 0
fi

# Install check. A /clear keeps the process: it was offered at startup.
[ "$source" = clear ] && exit 0
data="$(btp_data_dir)"
[ -e "$data/install-declined" ] && exit 0

install="$(cd "$here/../scripts" && pwd)/install-btp.sh"
if found="$(btp_find_any)"; then
  version="$(btp_version "$found")" || version="unknown"
  what="The btp found at $found (version $version) is too old for it (it needs $BTP_MIN_VERSION or later with the session commands)."
else
  what="btp is not installed."
fi
emit "Balance ton port (plugin): this plugin needs the btp CLI to stop the idle dev servers of a session when it quits. $what

Before anything else, ask the user once whether to install the latest btp from the project's GitHub release (it downloads btp-<version>-macos-universal.tar.gz, checks its sha256 and its Developer ID signature, installs it to ~/.local/bin/btp, never uses sudo, and only replaces a btp this plugin installed).
- On yes, run: bash \"$install\" --data-dir \"$data\"
  and tell the user what it printed (it refuses on any mismatch; say why).
- On no, run: mkdir -p \"$data\" && touch \"$data/install-declined\"
  and tell the user they can install it later with /balance-ton-port:install.
Install nothing without that yes."
exit 0
