# Balance ton port: design

This document describes how Balance ton port works today (v0.1). It is for contributors and for anyone who wants to know exactly when the tool kills something, and when it never will.

## 1. Intent

Claude Code sessions start dev servers (Astro, wrangler, Metro, Vite, `serve`...) and leave them running. The session ends or crashes, the worktree is deleted, and the ports stay taken; children such as wrangler's `workerd` linger, and so do the resources they hold (database connections, file watchers).

Balance ton port cleans up automatically and asks only when it cannot be sure. Its rules, in order of importance:

- Never kill a server someone is still using.
- Never touch anything that was not started from a Claude Code session: a Metro started in a plain terminal, Postgres, Docker, system services.
- Cost next to nothing while idle: no subprocesses, no `lsof` loop, only direct system calls once per scan.

It ships as a macOS menu bar app and a `btp` CLI, both built on one Rust core, plus an agent skill that teaches coding agents to use `btp` instead of `lsof | xargs kill`.

## 2. Sessions

A Claude Code session is known through its session file: `<config dir>/sessions/<pid>.json`, written by Claude Code itself. The core reads `pid`, `sessionId`, `cwd`, `status` (`idle` or `busy`), `statusUpdatedAt`, `procStart` and the optional `name`. A file missing a required key, or that does not parse, is skipped.

Config dirs are found automatically:

- `~/.claude`;
- `$CLAUDE_CONFIG_DIR`, when set;
- every `~/.claude-*` directory that contains a `sessions/` folder.

`claude_config_dirs` in the config adds extra dirs on top of those; it never replaces them, so a `~/.claude-*` dir created later is still picked up. `btp config get` prints the full list actually read.

A session is live only if a process with that pid is running and its start time, formatted as Claude Code formats `procStart` (UTC, `ps -o lstart` style), matches the file exactly. A stale file whose pid now belongs to another process therefore never counts. Once a server is tracked, its session is judged alive from the process itself (pid and start time recorded at link time), not from the file, so a file that is briefly unreadable never makes a live session look dead.

The Claude Code CLI is recognised by its command line: argv[0] named `claude`, a runtime (`node`, `bun`, `deno`) running a script named `claude`, or the `@anthropic-ai/claude-code` npm package.

## 3. What a server is

A server is a process listening on a TCP port whose ancestry, when it is first seen, leads to a live Claude Code session. Linking happens at first sight because the link is lost later: when the session process dies, its children are reparented to launchd.

On each scan, for every listening socket not already tracked, the core walks the listener's ancestry and takes the first ancestor that matches a live session (pid and start time). The listener is not linked when:

- it is a session process itself;
- it or any process between it and the session is a Claude Code CLI (a nested session owns its own servers) or matches an excluded command;
- its port is excluded;
- its working directory cannot be read.

The link records the server's (pid, start time) as its identity, its port and command line, its session (id, pid, start time, display name) and its worktree: the nearest ancestor of the server's working directory holding a `.git` entry (a directory for a main checkout, a file for a linked worktree), or the working directory itself outside a repository.

A server is a process tree. A listener whose ancestor below the session is already a tracked server belongs to that server, and a tracked process's extra ports are part of it. If a child started listening before its parent, the two rows are folded into the topmost one on the next pass (this check runs on every pass). A Claude Code process found inside a server's tree, with its whole subtree, is left out of that server's pids and ports, so `btp kill <port>` never picks the outer server for a port the nested session's server holds.

## 4. Idle

**Port idle**: no established TCP connection on any port of the tree, counted for every process of the tree. A nested Claude Code subtree still counts toward activity here (it errs on keeping). Every pass that sees at least one established connection resets the server's idle clock.

**Session idle**: the session file says `status: idle`; the idle time is now minus `statusUpdatedAt`. A `busy` session is not idle.

## 5. Triggers

Each pass (every `scan_interval_s`, default 5 s), the pure function `decide` gives at most one action per tracked server:

1. **Process gone** (or its pid now belongs to another process): forget it. Nothing is signalled.
2. **Worktree deleted**: kill at once, no alert. Only a definite "does not exist" counts; a path that cannot be checked (permissions, unmounted volume) does not.
3. **Shared worktree**: if another live session's worktree is the same as the server's, do nothing (no kill, no alert). This covers a session resumed in the same worktree under a new process.
4. **Session dead**: kill once the port has been idle for `dead_session_idle_s` (default 1200 s, 20 min).
5. **Session alive**: when the larger of port idle and session idle reaches `alert_after_s` (default 1800 s, 30 min), raise an alert, unless the server was kept or already alerted.

The shared-worktree rule matches worktrees, not connections. A server that another session in a different worktree actively uses is still protected from trigger 4, because that trigger needs the port idle; it is not protected from trigger 2.

## 6. Exclusions

- `exclude_commands` (default `mcp`, `argent`, `postgres`, `redis-server`, `mysqld`, `mongod`): case-insensitive substrings of the full command line.
- `exclude_ports` (default empty): a server is excluded when its main port, or any port of its tree, is in the list.

Exclusions are checked when a listener is linked, again on every pass (a tracked server that now matches, or has an ancestor below its session that matches, is forgotten, never killed), and again right before each kill, with the config re-read from disk. When a server is killed, any descendant that matches an excluded command is spared with its subtree, as is any Claude Code process found in the tree: a database the dev server started is left running.

## 7. Killing

`kill_tree`:

1. Check that the server's pid still has the recorded start time; if not, send nothing.
2. Snapshot the server and its own descendants as (pid, start time), leaving out Claude Code and excluded subtrees.
3. SIGTERM to each, re-checking the start time right before each signal.
4. Poll every 100 ms until all are gone or `kill_grace_s` (default 5 s) has passed.
5. SIGKILL to the survivors, again re-checking each start time first.

A pid that was reused within the grace period is never hit, and pid 1 and the watcher's own process never are either. Kills run outside the state lock, since the grace period can last seconds. A server that refuses the signal (EPERM) stays tracked so a later pass can retry.

Manual kills only ever reach tracked servers: by port (any port the tree listens on now, else the recorded port), by pid (the server's own pid) or, from the app and its notifications, by the exact (pid, start time) of the row. An untracked port or pid is refused (`btp kill` exits 1).

Every kill is recorded with port, command, worktree, reason (`worktree_deleted`, `dead_session_idle` with the idle time, or `manual`) and time: the last 5 in the state file (shown in the menu), all of them appended to `kills.log`.

## 8. Alerts

A pass does not consume alerts: it reports every server due one, on every pass, until the alert is marked delivered. The app posts a notification with Kill and Keep buttons and marks the alert only once Notification Center accepted it. When notifications are not allowed, nothing is marked, the server's row says "notifications off", and the alert goes out once they are allowed.

- **Kill** kills that exact process, asking the core rather than the in-memory list, so it works after an app relaunch. If the server changed since the notification, the app says there is nothing to kill.
- **Keep** marks the server: no more alerts until it restarts (a restart is a new process, so a new row).

`btp watch` prints each alert once and marks it as delivered. `btp sweep` ignores the alert trigger without marking anything, so the app still alerts later.

## 9. State, config and locking

Everything lives in `~/Library/Application Support/balance-ton-port/` (overridable with `BTP_DATA_DIR`):

| File | Content |
|---|---|
| `config.toml` | Settings, shared by the app and the CLI |
| `state.json` | Tracked servers (links, idle clocks, Keep and alerted marks) and the last 5 kills |
| `state.lock` | Exclusive lock held while `state.json` is read and rewritten |
| `kills.log` | One JSON line per kill |
| `watch.lock` | Held by the single watcher, with its name inside |

`state.json` is written through a temp file and a rename. A corrupt file is moved to `state.json.corrupt` and the state starts empty; any other read error is returned so the state is never overwritten blindly.

Config minimums: 60 s for `dead_session_idle_s` and `alert_after_s`, 1 s for `kill_grace_s` and `scan_interval_s`. `btp config set` and Settings refuse lower values; values read from the file are raised to the minimum with a note on stderr, printed once per change of the file. Saves are atomic, and the last writer wins. A file that does not parse never silently becomes the defaults: a running watcher keeps its last valid config (a fresh start uses the defaults), the app shows the error, and `btp config` refuses to read or write until it is fixed.

**One watcher.** Only the holder of `watch.lock` runs killing passes. The app tries to take it on every pass; while `btp watch` holds it, the app only observes (links, activity and forgets are still recorded, no kill, no alert) and shows the list read-only, but its manual Kill button still works. `btp watch` refuses to start while the app holds the lock. `btp list`, `btp kill` and `btp sweep` work without any watcher: they run the same pass (link, observe, forget) on demand.

## 10. Known limitations

- A Claude Code process with no session file in a watched dir (a `CLAUDE_CONFIG_DIR` whose `sessions/` folder is shared with another dir, an older Claude Code) is ignored: its servers are never tracked, so never killed, and never cleaned up either.
- A server started in the background (`cmd &`) inside one shell command is reparented to launchd before the first scan and is not tracked. More generally, anything started and orphaned between two scans is missed.
- Command exclusions are substrings of the whole command line, paths included: a project path containing `mcp` excludes its servers. That errs on the safe side.
- Port activity is sampled once per pass. A short request that opens and closes between two passes is not seen, so a server used only by brief, rare requests can look idle.
- The shared-worktree rule compares worktree roots only (see section 5).
- The kill log grows without bound, and the same kill can occasionally be recorded twice.
- macOS only. The platform layer is behind a trait so that a Linux implementation (`/proc`) can follow.

## 11. Architecture

```
crates/btp-core   Rust library: platform layer, sessions, linking, decide, kill, state, config
crates/btp-cli    the `btp` binary (clap)
crates/btp-ffi    UniFFI wrapper around the core for Swift
app/              SwiftUI menu bar app (XcodeGen project)
skills/           the agent skill
```

- `btp-core`: `platform` (a `Platform` trait; the macOS implementation uses libproc and sysctl for listening sockets, process info, ancestry, descendants and per-socket TCP states, plus `kill(2)`), `sessions` (session files and config dir discovery), `worktree`, `decide` (pure decision logic, unit-tested with fake clocks and fake process tables), `watcher` (link, refresh, observe, execute, the watch lock), `state`, `config`, and `labels`/`display` for readable names (`Expo (Metro)`, `repo › worktree`).
- `btp-ffi`: a `Core` object exposing `tick`, `observe`, `kill_port`, `kill_server(pid, start_time)`, `mark_alerted`, `keep`, `config`/`set_config`, `recent_kills` and `acquire_watch`. `scripts/build-xcframework.sh` builds a universal static library, packs it as `BtpCore.xcframework` and generates the Swift bindings.
- The app: `MenuBarExtra` in window style, `LSUIElement` (no Dock icon), launch at login through `SMAppService`, notifications with actions through `UNUserNotificationCenter`. It runs one pass at a time on a background queue and schedules the next one `scan_interval_s` after the previous one finished. One process, no daemon to install. macOS 14 or later.

## 12. Release pipeline

CI runs fmt, clippy, the Rust tests and the Swift tests on every push and pull request. A `v*` tag runs the release workflow: it checks that the tag matches the CLI and app versions, builds a universal `btp` and app, signs them with a Developer ID, notarizes and staples the app, packs it into `BalanceTonPort-<v>.dmg` (signed, notarized and stapled too), and publishes a GitHub release with the CLI tarball, the dmg, their `.sha256` files and the Homebrew formula and cask filled with the version and checksums; a tag with a hyphen is a prerelease, and nothing is published unless every step passed. See [RELEASING.md](../RELEASING.md) for the secrets and the steps.
