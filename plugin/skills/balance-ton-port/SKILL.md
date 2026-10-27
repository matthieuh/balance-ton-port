---
name: balance-ton-port
description: Use before starting or after finishing a local dev server, or when a port is already in use
---

# Balance ton port

`btp` tracks the dev servers started from coding-agent sessions (Claude Code, Codex CLI: Astro, wrangler, Metro, Vite, `serve`...) and kills the leftovers. Use it instead of improvising with `lsof`.

## Before starting a dev server

Start it through btp: `btp run -- <cmd>` (for example `btp run -- npm run dev`). It checks the port, reuses a server that already runs the same command here, and keeps the server's logs. Run it in the background as your tool allows; without a terminal it never prompts. Read the `btp:` line it prints on stderr, it holds the real address:

- `btp: storefront on http://localhost:3001 (3000 is used by admin, Codex session)`: it ran on another port. Use that address, not the one you expected.
- `btp: storefront already running on http://localhost:3000 (started by Claude 12 min ago), reusing it`: nothing was started (exit 0). Use that address.
- `btp: freed :3000 (...)`: it stopped a leftover that held the port, then started yours.
- Exit code 3, `btp: port 3000 is used by <holder>. Ask the user, ...`: btp did not resolve the conflict and started nothing. Ask the user, then rerun with `--free`, `--reroute` or a free port (only the options that would work are offered). Do not pick for them.

Options: `--free` frees an idle leftover, `--reroute` never touches the holder, `--new` starts a second copy, `--no-check` skips the check. Other exit codes are the server's own; 2 is a usage error.

Keep the `btp:` line when you background it: it is printed once, on stderr. Let your tool's background mode capture the output and read it, or run `btp run -- npm run dev > "${TMPDIR:-/tmp}/<name>-dev.log" 2>&1 &` and read the first lines of that file (never put the file inside the project). If you lost the line, `btp list --json` shows the server's real `port` once it listens; if the command exited with 3, nothing was started.

A Claude Code hook may refuse a dev command. If the same project is already running, use the address it gives instead of starting another copy. If the port is taken, rerun it through `btp run -- <cmd>` or pass the free port it suggests.

### Without `btp run` (older btp)

If `btp run` does not exist, run `btp list --json` and read the `servers` array. Each entry has `port` (its main port), `ports` (every port its process tree listens on), `command`, `worktree_name`, `agent` (`claude_code` or `codex`), `session_alive`, `idle_ms`, `kill_in_ms` and `state` (`active`, `idle` or `session dead`).

- A server of your own worktree already serves what you need: reuse its port, do not start a second one.
- The port is held by a leftover whose `state` is `session dead` AND whose `idle_ms` is at least 600000 (10 min without any connection): free it with `btp kill <port>`, then start yours.
- The port is held by a `session dead` server that is still in use (`idle_ms` under 600000): the user is probably still using it (a browser tab). Leave it alone and pick another port; the app kills it once it has been idle long enough.
- The port is held by another live session's server (`session_alive` is `true`, other worktree): never touch it. Pick another port.
- The port is not in the list but is taken: it was not started from a coding-agent session, so `btp` does not manage it. Do not kill it, pick another port.

## When the page is blank or errors

Read the server's output before guessing: `btp logs <port> --errors` (only the error lines, with context). `btp logs <port>` shows the last 200 lines, `-n N` changes that, `-f` follows. It works for servers started through `btp run`; `has_logs` in `btp list --json` says which.

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
btp run -- npm run dev   # start a server: port check, reuse, logs (read the "btp:" line)
btp logs 3000 --errors   # the server's error lines (add -f to follow, -n N for N lines)
btp list                 # table of tracked servers
btp list --json          # { "servers": [...], "recent_kills": [...] }
btp kill 4322            # kill the tracked server on port 4322
btp kill --pid 12345     # kill a tracked server by pid
btp sweep --dry-run      # show what the automatic rules would kill
btp sweep                # apply them
btp note 5173 --keep "preview for the user"   # no idle alert, not offered after /clear
btp note 5173 --done     # once the session quit, stopped as soon as idle
btp recent               # the last kills and why
```

`recent_kills` entries carry a `reason` of `{"kind": "manual"}`, `{"kind": "worktree_deleted"}`, `{"kind": "dead_session_idle", "idle_ms": ...}`, `{"kind": "session_ended"}`, or `{"kind": "other"}` for a kind written by a newer btp.
