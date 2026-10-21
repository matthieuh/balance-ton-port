---
name: balance-ton-port
description: Use before starting or after finishing a local dev server, or when a port is already in use
---

# Balance ton port

`btp` tracks the dev servers started from Claude Code sessions (Astro, wrangler, Metro, Vite, `serve`...) and kills the leftovers. Use it instead of improvising with `lsof`.

## Before starting a dev server

Run `btp list --json` and read the `servers` array. Each entry has `port`, `command`, `worktree_name`, `session_alive`, `idle_ms`, `kill_in_ms` and `state` (`active`, `idle` or `session dead`).

- A server of your own worktree already serves what you need: reuse its port, do not start a second one.
- The port is held by a leftover (`session_alive` is `false`): free it with `btp kill <port>`, then start yours.
- The port is held by another live session's server (`session_alive` is `true`, other worktree): never touch it. Pick another port.
- The port is not in the list but is taken: it was not started from a Claude Code session, so `btp` does not manage it. Do not kill it, pick another port.

## When you finish a task

Run `btp sweep`. It kills only the servers whose worktree was deleted, or whose session is dead and idle. Add `--dry-run` to see what it would kill first.

## Rules

- Never use `lsof | xargs kill`, `kill -9` or `pkill` on a port or a dev server. Use `btp kill`.
- Never kill another live session's server.
- `btp kill <port>` (or `btp kill --pid <pid>`) only works on tracked servers. Exit code 1 means the target is unknown: leave it alone.

## Commands

```bash
btp list                 # table of tracked servers
btp list --json          # { "servers": [...], "recent_kills": [...] }
btp kill 4322            # kill the tracked server on port 4322
btp kill --pid 12345     # kill a tracked server by pid
btp sweep --dry-run      # show what the automatic rules would kill
btp sweep                # apply them
```

`recent_kills` entries carry a `reason` of `{"kind": "manual"}`, `{"kind": "worktree_deleted"}` or `{"kind": "dead_session_idle", "idle_ms": ...}`.
