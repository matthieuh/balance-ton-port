#!/bin/bash
# Balance ton port, SessionEnd hook: stops the idle dev servers of the
# Claude Code process that quits (`btp session-end`).
#
# Claude Code shows nothing a SessionEnd hook prints (except stderr on exit
# 2, as an error) and gives it about 1.5 s. So this hook prints nothing,
# always exits 0, and stops waiting after 1.2 s; `btp session-end` itself
# stays within about 0.9 s, hands its SIGKILL stage to a detached process,
# and posts the result as a macOS notification.

exec 2>/dev/null
# Wall-clock bound for the whole hook, whatever the steps below cost.
sleep 1.2 &
timer=$!
trap 'kill "$timer"' EXIT
payload="$(cat)"
exec </dev/null >/dev/null

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" || exit 0
# shellcheck source=SCRIPTDIR/../scripts/lib.sh
. "$here/../scripts/lib.sh" || exit 0

reason="$(btp_json_field reason "$payload")"
session_id="$(btp_json_field session_id "$payload")"

# /clear and /resume keep the process: nothing to stop (btp agrees, this
# only saves starting it).
case "$reason" in
  "" | clear | resume) exit 0 ;;
esac

# Missing or too old: stay silent, the app or `btp watch` cleans up later.
bin="$(btp_find)" || exit 0

# The hook runs as a child of the claude process (checked on Claude Code
# 2.1.308), so $PPID is the session's process.
args=(session-end --session-pid "$PPID" --reason "$reason")
[ -n "$session_id" ] && args+=(--session-id "$session_id")

"$bin" "${args[@]}" &
child=$!
while kill -0 "$child" && kill -0 "$timer"; do
  sleep 0.05
done
exit 0
