---
name: balance-ton-port
description: Use before starting or after finishing a local dev server, or when a port is already in use
---

# Balance ton port

`btp` tracks the dev servers started from Claude Code sessions (Astro, wrangler, Metro, Vite, `serve`...) and kills the leftovers. Use it instead of improvising with `lsof`.

## Before starting a dev server

Run `btp list --json` and read the `servers` array. Each entry has `port` (its main port), `ports` (every port its process tree listens on), `command`, `worktree_name`, `session_alive`, `idle_ms`, `kill_in_ms` and `state` (`active`, `idle` or `session dead`).

- A server of your own worktree already serves what you need: reuse its port, do not start a second one.
- The port is held by a leftover whose `state` is `session dead` AND whose `idle_ms` is at least 600000 (10 min without any connection): free it with `btp kill <port>`, then start yours.
- The port is held by a `session dead` server that is still in use (`idle_ms` under 600000): the user is probably still using it (a browser tab). Leave it alone and pick another port; the app kills it once it has been idle long enough.
- The port is held by another live session's server (`session_alive` is `true`, other worktree): never touch it. Pick another port.
- The port is not in the list but is taken: it was not started from a Claude Code session, so `btp` does not manage it. Do not kill it, pick another port.

## Tell btp your intent

When the session quits, btp stops nothing at once: the servers still running were kept on purpose, and each is stopped once unused for 20 minutes (the dead-session rule). Tell it what you meant a server for, right after you start it or once you are done with it:

- `btp note <port> --keep "<why>"` when you start a server for the user to look at (a preview, a demo): it gets no idle alert and is never offered for cleanup in a later conversation. Give a short reason the user will recognise, for example `btp note 5173 --keep "preview of the new pricing page"`.
- `btp note <port> --done` when the check you started it for is finished: once the session has quit, it is stopped as soon as it is idle, without waiting the 20 minutes and without asking.

A note only applies to a server of your own session (exit 1 otherwise: leave it alone). It never stops a server that is in use, excluded, or shared with another live session.

## When you finish a task

Run `btp sweep`. It kills only the servers whose worktree was deleted, or whose session is dead and idle. Add `--dry-run` to see what it would kill first.

## Rules

- Never use `lsof | xargs kill`, `kill -9` or `pkill` on a port or a dev server. Use `btp kill`.
- Never kill another live session's server.
- Never kill a server that is in use (`idle_ms` under 600000), even when its session is dead.
- `btp kill <port>` (or `btp kill --pid <pid>`) only works on tracked servers. Exit code 1 means the target is unknown: leave it alone.

## Commands

```bash
btp list                 # table of tracked servers
btp list --json          # { "servers": [...], "recent_kills": [...] }
btp kill 4322            # kill the tracked server on port 4322
btp kill --pid 12345     # kill a tracked server by pid
btp sweep --dry-run      # show what the automatic rules would kill
btp sweep                # apply them
btp note 5173 --keep "preview for the user"   # spare it when the session quits
btp note 5173 --done     # stop it when the session quits, once idle
btp recent               # the last kills and why
```

`recent_kills` entries carry a `reason` of `{"kind": "manual"}`, `{"kind": "worktree_deleted"}`, `{"kind": "dead_session_idle", "idle_ms": ...}` or `{"kind": "session_ended"}` (stopped when its session quit).
