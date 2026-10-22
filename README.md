# Balance ton port

Claude Code sessions start dev servers (Astro, wrangler, Metro, Vite, `serve`...) and leave them behind. The session ends or crashes, the worktree is deleted, and the ports stay taken. Balance ton port cleans up automatically and asks only when it cannot be sure.

It is a macOS menu bar app plus a `btp` CLI, both on one Rust core, and an agent skill.

## What it does

Every 5 seconds it links new listening servers to the Claude Code session that started them, then applies three triggers:

1. **Worktree deleted**: the server is killed at once.
2. **Session dead and port idle for 20 min**: the server is killed.
3. **Session alive but idle for 30 min**: a notification with Kill and Keep buttons (app only).

A server is a process tree: the listening process started under the session plus its children (for example Astro or wrangler with their `workerd` workers). A child listening on its own port is part of its parent's server, and "idle" means no established connection on any port of the whole tree. Kill is SIGTERM on that tree, then SIGKILL after 5 s for survivors; a Claude Code process found in the tree, or a process matching an excluded command (a `postgres` the dev server started, say), is never signalled, nor is anything it runs. The menu bar lists running servers and the last 5 kills with their reason.

## How it decides

- Only servers linked, at first sight, to a live Claude Code session are tracked; a Claude Code session is recognised by its session file and its process start time.
- Worktree deleted: killed at once. Session dead: killed after 20 min without any established connection on its process tree. Session alive and idle 30 min: an alert, never a silent kill.
- A server whose worktree another live session uses is left alone.
- Excluded commands and ports are re-checked on every scan and right before each kill; excluded children and nested Claude Code processes are spared.
- Every signal is preceded by a start time check, so a reused pid is never hit.

The full design, limitations included, is in [docs/design.md](docs/design.md).

## Install

Menu bar app: download `BalanceTonPort-<version>.dmg` from [GitHub Releases](https://github.com/matthieuh/balance-ton-port/releases), open it and drag Balance ton port to Applications. The image and the app are signed with a Developer ID and notarized. The `btp` CLI is attached to the same release as `btp-<version>-macos-universal.tar.gz`.

Homebrew (coming with the first release):

```bash
brew install matthieuh/tap/btp                   # CLI
brew install --cask matthieuh/tap/balance-ton-port   # menu bar app
```

From source today (macOS, Xcode, rustup with the aarch64 and x86_64 darwin targets, XcodeGen):

```bash
cargo install --path crates/btp-cli              # the btp CLI

scripts/build-xcframework.sh                     # Rust core + Swift bindings
(cd app && xcodegen generate)
xcodebuild -project app/BalanceTonPort.xcodeproj -scheme BalanceTonPort build
```

## CLI

```bash
btp list [--json]        # tracked servers; --json has "servers" and "recent_kills"
btp kill <port>          # kill a tracked server by port
btp kill --pid <pid>     # ... or by pid
btp sweep [--dry-run]    # apply triggers 1 and 2 once; --dry-run kills nothing
btp watch                # watch in the foreground until Ctrl-C
btp config get           # print the effective configuration
btp config set <key> <n> # set a numeric key
```

Only servers Balance ton port tracks can be killed; an unknown port or pid exits 1. `btp kill <port>` accepts any port of a server's process tree. `btp watch` refuses to start while the app is already watching, and prints each alert once instead of notifying. Without the app, `btp sweep` applies triggers 1 and 2 only (no alerts).

## Configuration

`~/Library/Application Support/balance-ton-port/config.toml`, shared by the app and the CLI. Edit it, use the app's Settings, or `btp config set`.

| Key | Default | Meaning |
|---|---|---|
| `dead_session_idle_s` | 1200 | Idle time before a dead session's server is killed (at least 60) |
| `alert_after_s` | 1800 | Idle time before the Kill / Keep alert (at least 60) |
| `kill_grace_s` | 5 | Delay between SIGTERM and SIGKILL (at least 1) |
| `scan_interval_s` | 5 | Scan period (at least 1) |
| `claude_config_dirs` | `[]` | Extra Claude config dirs only. `~/.claude`, `$CLAUDE_CONFIG_DIR` and every `~/.claude-*` with a `sessions/` folder are found automatically |
| `exclude_commands` | `mcp`, `argent`, `postgres`, `redis-server`, `mysqld`, `mongod` | Never tracked or killed when the command line contains one of these (case-insensitive) |
| `exclude_ports` | `[]` | Never tracked or killed when the server, or one of its child processes, listens on one of these ports |

An exclusion matches the whole command line, paths included: a project path containing "mcp" excludes its servers. That errs on the safe side. Exclusions are checked on every scan, not only when a server is first seen: adding one stops tracking a server that matches, it is never killed.

Values below their minimum are refused by `btp config set` and the Settings window, and raised to the minimum when read from the file (with a note on stderr, once per change of the file). If `config.toml` cannot be parsed (one mistyped value is enough), nothing falls back to the defaults silently: a running watcher keeps its last valid settings (a fresh start uses the defaults), the app shows the error in its menu, the CLI prints it, and `btp config get` / `set` refuse to touch the file until it is fixed. Saves are atomic. The app and `btp config set` both write the whole file: the last one to save wins.

## Agent skill

Teaches agents to run `btp list --json` before starting a server, reuse or free ports with `btp`, and `btp sweep` at the end of a task, instead of `lsof | xargs kill`.

```bash
cp -r skills/balance-ton-port ~/.claude/skills/
```

If you use a custom `CLAUDE_CONFIG_DIR`, copy it into that directory's `skills/` folder instead.

## Safety rules

- Never kills a process that was not started from a live Claude Code session.
- A server whose worktree is used by another live session is never touched.
- Claude Code processes themselves are never tracked, and never signalled when found inside a server's process tree.
- A child matching an excluded command (a database started by the dev server) is never signalled when its server is killed.
- Excluded commands and ports are re-checked on every scan and right before each kill.
- Start times are re-checked before every signal, so a reused pid is never hit.
- `btp sweep --dry-run` prints what would be killed.

## Notifications

The idle alert (trigger 3) needs notifications allowed for Balance ton port. The app asks once, at its first launch; if that prompt was dismissed or refused, macOS refuses every later notification without asking again. The app then says "notifications off" on the row of each server due an alert, and keeps the alert pending (it is sent once notifications are allowed). To allow them: System Settings > Notifications > Balance ton port > Allow notifications (Settings also has a button that opens that page). Launch the app from Finder or with `open "Balance ton port.app"`, ideally from `/Applications`, so macOS registers it and lists it there.

## Known limitations

- A Claude Code process that has no session file in a watched config dir (for example a `CLAUDE_CONFIG_DIR` whose `sessions/` folder is shared with another dir, or an older Claude Code) is not watched: the servers it starts are not tracked and never killed. That is the safe direction: nothing is killed, but nothing is cleaned up for that session either.
- A server started in the background (`cmd &`) inside one shell command is reparented to launchd before the first scan and is not tracked.

## Releasing

Maintainers: see [RELEASING.md](RELEASING.md) for CI, the signing secrets and how to cut a release.

## License

MIT
