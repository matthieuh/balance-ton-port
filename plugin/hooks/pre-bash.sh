#!/bin/bash
# Balance ton port, PreToolUse hook on the Bash tool. Hands the payload to
# `btp hook pre-bash`, which checks the port when the command starts a known
# dev server: it frees an idle dead-session server's port, or denies the
# command with a message for Claude (printed as the permission decision).
#
# Prints nothing and exits 0 on every other path: btp missing, older than
# 0.5.0 (the first with `btp hook pre-bash`), or any error. It never blocks
# the agent because of btp.
#
# The hook's timeout is 10 s: the port check takes well under a second and
# freeing a port waits at most `kill_grace_s` (5 s by default) for the kill.
# A `kill_grace_s` above 7 s may hit the timeout; Claude Code then lets the
# command run, without blocking.

exec 2>/dev/null

# The first btp that has `btp hook pre-bash`.
PRE_BASH_MIN_VERSION="0.5.0"

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" || exit 0
# shellcheck source=SCRIPTDIR/../scripts/lib.sh
. "$here/../scripts/lib.sh" || exit 0

bin="$(btp_find)" || exit 0
version="$(btp_version "$bin")" || exit 0
btp_version_ge "$version" "$PRE_BASH_MIN_VERSION" || exit 0

"$bin" hook pre-bash || true
exit 0
