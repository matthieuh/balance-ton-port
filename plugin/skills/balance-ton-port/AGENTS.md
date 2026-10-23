## Dev servers: Balance ton port (`btp`)

`btp` tracks the dev servers started from coding-agent sessions (Astro, wrangler, Metro, Vite, `serve`...) and kills the leftovers. Use it instead of improvising with `lsof`.

### Before starting a dev server

Run `btp list --json` and read the `servers` array (`port`, `ports`, `command`, `worktree_name`, `agent`, `session_alive`, `idle_ms`, `state`).

- A server of your own worktree already serves what you need: reuse its port.
- The port is held by a leftover whose `state` is `session dead` and whose `idle_ms` is at least 600000: free it with `btp kill <port>`, then start yours.
- The port is held by a `session dead` server with `idle_ms` under 600000, or by another live session's server: never touch it, pick another port.
- The port is taken but not in the list: `btp` does not manage it. Do not kill it, pick another port.

### Tell btp your intent

- `btp note <port> --keep "<why>"` for a server you start for the user to look at (a preview, a demo).
- `btp note <port> --done` when the check you started it for is finished.

A note only applies to a server of your own session (exit 1 otherwise: leave it alone).

### When you finish a task

Run `btp sweep` (add `--dry-run` to see first). It kills only servers whose worktree was deleted, or whose session is dead and idle.

### Rules

- Never use `lsof | xargs kill`, `kill -9` or `pkill` on a port or a dev server. Use `btp kill`.
- `btp kill <port>` only works on tracked servers. Exit code 1 means the target is unknown: leave it alone.
- Never kill another live session's server, nor one that is still in use.

### Inside the Codex sandbox

The default sandbox refuses to let a server listen on a port, and makes btp's data dir (`~/Library/Application Support/balance-ton-port`) read-only.

- Start dev servers with escalation (or run Codex with full access).
- Run `btp kill`, `btp sweep` and `btp note` with escalation.
- `btp list` works read-only, but shows the last saved state (`state` is `saved`), not the live one.
