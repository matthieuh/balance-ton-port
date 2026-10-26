## Dev servers: Balance ton port (`btp`)

`btp` tracks the dev servers started from coding-agent sessions (Astro, wrangler, Metro, Vite, `serve`...) and kills the leftovers. Use it instead of improvising with `lsof`.

### Before starting a dev server

Start it with `btp run -- <cmd>` (for example `btp run -- npm run dev`), in the background as your tool allows (without a terminal it never prompts). It checks the port, reuses a server already running the same command here, and keeps the logs. Read the `btp:` line on stderr for the real address:

- `btp: ... on http://localhost:3001 (3000 is used by ...)`: it ran on another port, use that address.
- `btp: ... already running on http://localhost:3000 ..., reusing it`: nothing started (exit 0), use that address.
- `btp: freed :3000 (...)`: a leftover was stopped, then yours started.
- Exit 3 (`btp: port 3000 is used by <holder>. Ask the user, ...`): nothing started. Ask the user, then rerun with `--free`, `--reroute` or a free port.

Other options: `--new` (second copy), `--no-check`. Other exit codes are the server's own.

Keep the `btp:` line when you background it: it is printed once, on stderr. Let your tool's background mode capture the output and read it, or run `btp run -- npm run dev > "${TMPDIR:-/tmp}/<name>-dev.log" 2>&1 &` and read the first lines of that file (never put the file inside the project). If you lost the line, `btp list --json` shows the server's real `port` once it listens; if the command exited with 3, nothing was started.

If a hook refuses a dev command because the same project is already running, use the address it gives. If it refuses because the port is taken, rerun it through `btp run -- <cmd>` or pass the free port it suggests.

Without `btp run` (older btp), run `btp list --json` and read the `servers` array (`port`, `ports`, `command`, `worktree_name`, `agent`, `session_alive`, `idle_ms`, `state`).

- A server of your own worktree already serves what you need: reuse its port.
- The port is held by a leftover whose `state` is `session dead` and whose `idle_ms` is at least 600000: free it with `btp kill <port>`, then start yours.
- The port is held by a `session dead` server with `idle_ms` under 600000, or by another live session's server: never touch it, pick another port.
- The port is taken but not in the list: `btp` does not manage it. Do not kill it, pick another port.

### When the page is blank or errors

Run `btp logs <port> --errors` before guessing (`btp logs <port>` for the last 200 lines, `-n N`, `-f`).

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
