#!/bin/bash
# Balance ton port, SessionEnd hook: records the dev servers the Claude
# Code process leaves running when it quits (`btp session-end`). It never
# stops any: they follow the dead-session rule (a detached `btp
# _sweep-later` applies it when neither the app nor `btp watch` runs).
#
# Claude Code shows nothing a SessionEnd hook prints (except stderr on exit
# 2, as an error) and gives it about 1.5 s. So this hook prints nothing and
# always exits 0. All the work (finding btp, checking its version, running
# `btp session-end`) happens in a child detached into its own session, so
# Claude Code quitting cannot kill it midway; the hook waits for it at
# most 1.2 s from its own start. `btp session-end` takes tens of ms and
# posts what was kept as a macOS notification.
#
#   session-end.sh            the hook (payload on stdin)
#   session-end.sh --run PID REASON [SESSION_ID]   the detached child

exec 2>/dev/null

# BTP_HOOK_DEBUG=1 (off by default): one line per step, appended to
# session-end.log in btp's data dir, next to the trace `btp session-end`
# writes there.
debug_log() {
  [ "${BTP_HOOK_DEBUG:-}" = 1 ] || return 0
  local dir="${BTP_DATA_DIR:-$HOME/Library/Application Support/balance-ton-port}"
  mkdir -p "$dir" && printf '%s hook[%s] %s\n' "$(date +%s)" "$$" "$*" >>"$dir/session-end.log"
}

if [ "${1:-}" = --run ]; then
  exec </dev/null >/dev/null
  claude_pid="${2:-}" reason="${3:-}" session_id="${4:-}"
  # Only as the leader of its own process group (the setsid below worked):
  # the watchdog's `kill 0` must never reach Claude Code's group.
  [ "$(ps -o pgid= -p "$$" | tr -d ' ')" = "$$" ] || {
    debug_log "not detached from Claude Code's process group: nothing done"
    exit 0
  }
  # A hung btp never lingers: the whole child's process group goes after
  # 15 s (`btp session-end` detaches its waiter into another session).
  (
    sleep 15
    kill -TERM 0
  ) &
  watchdog=$!
  here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" || exit 0
  # shellcheck source=SCRIPTDIR/../scripts/lib.sh
  . "$here/../scripts/lib.sh" || exit 0
  # Missing or too old: stay silent, the app or `btp watch` cleans up later.
  if bin="$(btp_find)"; then
    debug_log "running $bin session-end (parent $PPID)"
    args=(session-end "--session-pid=$claude_pid" "--reason=$reason")
    [ -n "$session_id" ] && args+=("--session-id=$session_id")
    "$bin" "${args[@]}"
  else
    debug_log "no usable btp (missing or older than $BTP_MIN): nothing done"
  fi
  kill "$watchdog"
  exit 0
fi

# Wall-clock bound for the whole hook, whatever the steps below cost.
sleep 1.2 &
timer=$!
trap 'kill "$timer"' EXIT

payload="$(cat)"
exec </dev/null >/dev/null

# The same parsing as btp_json_field in lib.sh, inline so the hook itself
# runs nothing but bash before handing over.
field() {
  local re="\"$1\"[[:space:]]*:[[:space:]]*\"([^\"\\\\]*)\""
  [[ "$payload" =~ $re ]] && printf '%s' "${BASH_REMATCH[1]}"
}
reason="$(field reason)"
session_id="$(field session_id)"

if [ "${BTP_HOOK_DEBUG:-}" = 1 ]; then
  files=""
  for d in "${CLAUDE_CONFIG_DIR:-}" "$HOME/.claude" "$HOME"/.claude-*; do
    [ -n "$d" ] && [ -f "$d/sessions/$PPID.json" ] && files="$files $d/sessions/$PPID.json"
  done
  debug_log "reason=$reason session_id=$session_id ppid=$PPID ppid_command=$(ps -o comm= -p "$PPID") session file:${files:- missing}"
fi

# /clear and /resume keep the process: nothing to stop (btp agrees, this
# only saves starting it).
case "$reason" in
  "" | clear | resume) exit 0 ;;
esac

# The hook runs as a child of the claude process (checked on Claude Code
# 2.1.308), so $PPID is the session's process.
self="${BASH_SOURCE[0]}"
if [ -x /usr/bin/perl ]; then
  /usr/bin/perl -MPOSIX -e '(POSIX::setsid() // -1) > 0 or exit 0; exec @ARGV or exit 0' \
    /bin/bash "$self" --run "$PPID" "$reason" "$session_id" &
else
  # No perl: at least its own process group, away from Claude Code's.
  set -m
  /bin/bash "$self" --run "$PPID" "$reason" "$session_id" &
  set +m
fi
child=$!
while kill -0 "$child" && kill -0 "$timer"; do
  sleep 0.05
done
exit 0
