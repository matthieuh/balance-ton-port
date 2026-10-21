# Balance ton port: design

Date: 2026-10-21
Status: approved by the user on 2026-10-21
Repo: `github.com/matthieuh/balance-ton-port` (new, open source)

## 1. Intent

Claude Code sessions start dev servers (Astro, wrangler, Metro, Vite, `serve`...) and leave them running: the session ends, crashes, or the worktree is deleted, and the ports stay taken, wrangler's `workerd` children linger, and shared resources behind them (a database pooler's connections, for instance) stay held. Today the only remedies are manual: `lsof` + `kill`, or menu bar apps that list ports and wait for a click.

Balance ton port cleans up **automatically**, and asks only when it cannot be sure.

- Who: a developer running several Claude Code sessions and git worktrees in parallel (first user: the author). Later: Linux machines and CI runners.
- Success: leftover dev servers disappear on their own without ever killing a server someone is still using; the menu bar shows what runs, where, and what was killed and why.

## 2. Goals, non-goals, anti-goals

Goals
- Automatic kill of dev servers whose worktree was deleted, or whose Claude Code session died and whose port went idle.
- An alert with [Kill] / [Keep] for servers of a living but idle session.
- A macOS menu bar app, very light (asleep between events).
- A CLI `btp` usable by humans and by agents, and a skill that teaches agents to use it.
- Open source, installed with Homebrew.

Non-goals (v1)
- Linux and CI (next step; the core is designed for them now).
- A full window, a long history, a web dashboard.
- Managing servers started outside Claude Code.

Anti-goals
- Killing a server the user is still using (browser tab open on it, tests running against it).
- Touching anything not started from a Claude Code session: a terminal Metro, Postgres, Docker, system services.
- Background work that costs noticeable CPU or battery (no `lsof` loop, no shelling out; only cheap direct system calls).

## 3. Decisions taken with the user

| Topic | Decision |
|---|---|
| Name | Balance ton port, repo `balance-ton-port` under the author's GitHub account |
| What a "session" is | A Claude Code session |
| Triggers | Combination: worktree deleted → immediate kill; session dead + port idle → automatic kill; session alive but idle or port idle → alert |
| Alert | macOS notification with two buttons, [Kill] and [Keep] |
| Interface | Menu bar only (no Dock icon, no window): server list + "Recently killed" (last 5) + Settings / Quit |
| Stack | Rust core + `btp` CLI; macOS app in Swift/SwiftUI on top of the core (GPUI dropped: no menu bar support upstream; GPUIX dropped: brings a JS runtime) |
| Order | v1 = macOS app + `btp` on macOS + agent skill; Linux and CI next |
| Distribution | Open source on GitHub, Homebrew (cask for the app, formula for `btp`), signed and notarized |
| Which servers | Everything a Claude session started, minus an exclusion list (not an allowlist of known dev tools) |
| Signing | HAPPYSOLVERS SAS Developer ID |

Menu as agreed:

```
⚓ Balance ton port
──────────────────────────────────
● 4322  astro dev   web-itinerairer   active      [Kill]
● 3010  wrangler    web-itinerairer   idle 12 min [Kill]
○ 8082  metro       dev-1b9694ee      session dead · kill in 18 min [Kill]
──────────────────────────────────
Recently killed
  4400  serve     worktree deleted          2 h ago
  3011  wrangler  dead session + idle 30 min  yesterday
──────────────────────────────────
Settings…   Quit
```

## 4. Behaviour

### What is watched
A **server** is a process listening on a TCP port whose process ancestry, at the time it was first seen, contains a Claude Code process, **minus an exclusion list** (user decision: everything a session started, not an allowlist of known dev tools, so new tools are covered). Its process tree (children such as `workerd`) belongs to it.

- Built-in exclusions (*my call*, extendable in Settings): MCP servers, Claude Code's own listeners (IDE, OAuth), the argent tool-server, databases (postgres, redis, mysql, mongod).
- **Shared servers**: a server that another live Claude session's worktree also uses (same worktree, or connections from another live session's processes) is never killed or alerted on (*my call*).
- **What counts as a Claude Code session** (*my call*): the `claude` CLI process (native binary or Node), wherever it runs: plain terminal, Superset, tmux, editor terminals. Claude Desktop's Code tab and Agent SDK hosts come later.
- **Discovery** (*my call*): a scan of listening sockets every 5 s through libproc (a few milliseconds, no subprocess), so a new server is linked while its session is still alive. Servers started while neither the app nor `btp watch` was running are linked if their session is still alive at the next scan, otherwise ignored.

The ancestry must be captured **while the session is alive**: once the Claude Code process dies, the server is reparented to `launchd` and the link is lost. So the core records server → session (and worktree = the server's working directory's git toplevel) as soon as it sees a new listener, and persists that mapping on disk so an app restart does not forget it.

Watched Claude Code config directories: `~/.claude` and every `CLAUDE_CONFIG_DIR` the user uses (the author has `~/.claude-perso`), configurable in Settings (*my call*).

### Triggers
1. **Worktree deleted** → kill now, no alert. "Deleted" = the server's recorded worktree directory no longer exists.
2. **Session dead** → the server enters a grace period; once its port has been **idle for 20 min** (*my call*, setting) it is killed.
3. **Session alive** and (session idle **or** port idle) for **30 min** (*my call*, setting) → notification:
   > **Balance ton port**
   > `astro dev` on port 4322 (worktree *web-itinerairer-public*) hasn't been used for 45 min.
   > [Kill] [Keep]

   [Keep] = no more alerts for that server until it restarts (*my call*).

**All delays are configurable** (user decision): the dead-session idle delay (default 20 min), the alert delay (default 30 min), the SIGKILL delay (default 5 s) and the scan interval (default 5 s). They live in one config file shared by the app and `btp` (`~/Library/Application Support/balance-ton-port/config.toml`, *my call*), editable from the app's Settings and with `btp config`.

Definitions (*my calls*):
- **Session idle**: the session's transcript file has not been written for the threshold.
- **Port idle**: no established connection to the port seen during the threshold.

### Killing
SIGTERM to the server's process group / tree, SIGKILL after 5 s for survivors (*my call*). Every kill is logged with port, command, worktree, reason, time; the last 5 show in the menu, the full log is a file.

## 5. Architecture

- **`btp-core` (Rust)**: discovery of listeners and process trees, session and worktree linking, idle tracking, trigger decisions, kill. Platform layer behind a trait: macOS now (libproc / `proc_pidinfo` / `proc_pidfdinfo`, kqueue `EVFILT_PROC NOTE_EXIT` for process death, FSEvents for worktree deletion), Linux later (`/proc`, inotify, pidfd).
- **`btp` (Rust CLI)**: `btp list [--json]`, `btp kill <port|pid>`, `btp sweep [--dry-run]` (applies the triggers once), `btp watch` (runs the watcher in the foreground; later the Linux service).
- **macOS app (Swift / SwiftUI)**: `MenuBarExtra` (window style for the custom rows), `LSUIElement` (no Dock icon), launch at login via `SMAppService`, notifications with actions via `UNUserNotificationCenter`. Links `btp-core` through UniFFI (one process, no daemon to install).
- **Ownership and state** (*my call*): one state file (server → session/worktree links, Keep marks, kill log) under `~/Library/Application Support/balance-ton-port/`, guarded by a file lock. On macOS the app is the watcher; `btp list`, `btp kill` and `btp sweep` work without the app (they read the state, and link servers of still-living sessions themselves); `btp watch` refuses to start if the app already watches. Without the app, trigger 3 (alert) is skipped: `btp sweep` applies triggers 1 and 2 only.
- **Agent skill** (`skills/balance-ton-port/SKILL.md`): before starting a dev server run `btp list` to check the port; if a server of the same worktree is already running reuse it; kill only leftovers (`btp kill`); never touch another live session's servers; `btp sweep` at the end of a task.

Performance budget (*my call*): near 0 % CPU when nothing happens, under 30 MB RAM for the app. Event-driven where the OS allows it; the only sampling is the port-idle check (connection states via libproc, every 30 s, *my call*).

## 6. Distribution

GitHub releases built in CI; Homebrew tap `matthieuh/homebrew-tap` with a cask (app) and a formula (`btp`) (*my call*: own tap first, homebrew-core later if it gets traction). App signed with the **HAPPYSOLVERS SAS** Developer ID (user decision; the existing paid account) and notarized from CI, credentials as repository secrets. License MIT (*my call*).

## 7. Safety rules
- Never kill a process that was not linked to a Claude Code session.
- `btp sweep --dry-run` prints what would be killed.
- A per-command or per-port exclusion list in Settings (*my call*).

## 8. Next (not v1)
- Linux: same core, `btp watch` as a systemd user service, desktop notifications.
- CI: `btp sweep` as the last step of a job.

## 9. Left to the builder
Exact crate and module split; how to map a Claude Code process to its transcript file (session id); the UniFFI build setup and how the xcframework is produced; persistence format of the server → session mapping; menu styling; test strategy (pure decision logic unit-tested with fake clocks and fake process tables).
