# Balance ton port: design

This document describes how Balance ton port works today. It is for contributors and for anyone who wants to know exactly when the tool kills something, and when it never will.

## 1. Intent

Coding-agent sessions (Claude Code, Codex CLI) start dev servers (Astro, wrangler, Metro, Vite, `serve`...) and leave them running. The session ends or crashes, the worktree is deleted, and the ports stay taken; children such as wrangler's `workerd` linger, and so do the resources they hold (database connections, file watchers).

Balance ton port cleans up automatically and asks only when it cannot be sure. Its rules, in order of importance:

- Never kill a server someone is still using.
- Never stop automatically anything an agent did not start: a Metro started in a plain terminal, Postgres, Docker, system services. The app lists such servers (section 9), but only you can stop them, one at a time, after a confirmation.
- Cost next to nothing while idle: no subprocesses, no `lsof` loop, only direct system calls once per scan.
- Stay off the network: the only calls besides localhost are the daily Sparkle update check and the download of an update, both from GitHub (no system profile sent).

It ships as a macOS menu bar app and a `btp` CLI, both built on one Rust core, plus an agent skill that teaches coding agents to use `btp` instead of `lsof | xargs kill`.

## 2. Sessions

Two agents are supported. Every session carries an `agent` (`claude_code` or `codex`, shown as `agent` in `btp list --json`; a state file from before Codex support loads as `claude_code`).

### Claude Code

A Claude Code session is known through its session file: `<config dir>/sessions/<pid>.json`, written by Claude Code itself. The core reads `pid`, `sessionId`, `cwd`, `status` (`idle` or `busy`), `statusUpdatedAt`, `procStart` and the optional `name`. A file missing a required key, or that does not parse, is skipped.

Config dirs are found automatically:

- `~/.claude`;
- `$CLAUDE_CONFIG_DIR`, when set;
- every `~/.claude-*` directory that contains a `sessions/` folder.

`claude_config_dirs` in the config adds extra dirs on top of those; it never replaces them, so a `~/.claude-*` dir created later is still picked up. `btp config get` prints the full list actually read.

A session is live only if a process with that pid is running and its start time, formatted as Claude Code formats `procStart` (UTC, `ps -o lstart` style), matches the file exactly. A stale file whose pid now belongs to another process therefore never counts. Once a server is tracked, its session is judged alive from the process itself (pid and start time recorded at link time), not from the file, so a file that is briefly unreadable never makes a live session look dead.

The Claude Code CLI is recognised by its command line: argv[0] named `claude`, a runtime (`node`, `bun`, `deno`) running a script named `claude`, or the `@anthropic-ai/claude-code` npm package.

### Codex CLI

Every native `codex` process (argv[0] path ending in `/codex`) running interactively (the TUI) or as `codex exec` / `codex review` is a session, with or without a thread lock; a `codex exec` started inside another live session belongs to that session instead. Codex is usually started through a shell wrapper that stays its parent; the wrapper is recognised as a wrapper and is never a session. `codex app-server` (the Codex desktop app) and the other service subcommands are never sessions.

Codex has no session file. The thread id, used as the conversation id, comes from the lock the process holds, `<CODEX_HOME>/thread-writer-locks/<uuid>.lock` (`CODEX_HOME` is read from the process environment, default `~/.codex`); a process holding several locks keeps the others as further conversations. Liveness uses the same rule as for Claude Code: pid plus process start time.

From the thread's rollout file (`<CODEX_HOME>/sessions/.../rollout-*-<uuid>.jsonl`) only line 1, the `session_meta` record, is read, for the thread id, the start timestamp, the cwd and the parent thread of a sub-thread. Nothing else of a rollout is read, parsed, stored or logged. When there is no lock yet, the newest rollout of the process's cwd stands in. The display name is the basename of the session cwd.

Codex writes its transcript only at the end of a turn, so a running turn cannot be told from an idle session: the status of a Codex session is always unknown and its session idle time is never known. A live Codex session's server is alerted on port idle only, like a busy Claude Code session's (trigger 5). It is also covered by the dead-session rule and the worktree-deleted rule.

## 3. What a server is

A server is a process listening on a TCP port whose ancestry, when it is first seen, leads to a live agent session (Claude Code or Codex CLI). Linking happens at first sight because the link is lost later: when the session process dies, its children are reparented to launchd.

On each scan, for every listening socket not already tracked, the core walks the listener's ancestry and takes the first ancestor that matches a live session (pid and start time). The listener is not linked when:

- it is a session process itself;
- it or any process between it and the session is an agent CLI (a nested Claude Code session owns its own servers; a nested `codex exec` stays with the outer session) or matches an excluded command;
- its port is excluded;
- its working directory cannot be read.

Linking is by ancestry only: a listener with no live session among its ancestors (a server reparented to launchd before it was seen, for example) is never linked, whatever its environment says. When the matched session is a Codex session, the `CODEX_THREAD_ID` of the listener's environment (or of the nearest process below the session that has one) picks which of that session's own threads the server belongs to; a value the session does not hold is ignored and the session's main thread is used. Environments are read only for a listener matched to a Codex session, for the current user's processes only, and only that one value is kept.

An agent CLI (Claude Code or Codex) is never tracked as a server, and is spared, with its whole subtree, in a kill tree.

The link records the server's (pid, start time) as its identity, its port and command line, its session (id, pid, start time, display name) and its worktree: the nearest ancestor of the server's working directory holding a `.git` entry (a directory for a main checkout, a file for a linked worktree), or the working directory itself outside a repository.

A server is a process tree. A listener whose ancestor below the session is already a tracked server belongs to that server, and a tracked process's extra ports are part of it. If a child started listening before its parent, the two rows are folded into the topmost one on the next pass (this check runs on every pass). A process of any agent CLI (Claude Code or Codex) found inside a server's tree, with its whole subtree, is left out of that server's pids and ports, so `btp kill <port>` never picks the outer server for a port the nested session's server holds.

## 4. Idle

**Port idle**: no established TCP connection on any port of the tree, counted for every process of the tree. A nested agent CLI subtree still counts toward activity here (it errs on keeping). Every pass that sees at least one established connection resets the server's idle clock.

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

Exclusions are checked when a listener is linked, again on every pass (a tracked server that now matches, or has an ancestor below its session that matches, is forgotten, never killed), and again right before each kill, with the config re-read from disk. When a server is killed, any descendant that matches an excluded command is spared with its subtree, as is any agent CLI process found in the tree: a database the dev server started is left running.

## 7. Killing

`kill_tree`:

1. Check that the server's pid still has the recorded start time; if not, send nothing.
2. Snapshot the server and its own descendants as (pid, start time), leaving out agent CLI and excluded subtrees.
3. SIGTERM to each, re-checking the start time right before each signal.
4. Poll every 100 ms until all are gone or `kill_grace_s` (default 5 s) has passed.
5. SIGKILL to the survivors, again re-checking each start time first.

A pid that was reused within the grace period is never hit, and pid 1 and the watcher's own process never are either. Kills run outside the state lock, since the grace period can last seconds. A server that refuses the signal (EPERM) stays tracked so a later pass can retry.

Manual kills only ever reach tracked servers: by port (any port the tree listens on now, else the recorded port), by pid (the server's own pid) or, from the app and its notifications, by the exact (pid, start time) of the row. An untracked port or pid is refused (`btp kill` exits 1).

Every kill is recorded with port, command, worktree, reason (`worktree_deleted`, `dead_session_idle` with the idle time, `port_conflict` or `manual`) and time: the last 5 in the state file (shown in the menu), all of them appended to `kills.log`.

## 8. Alerts

A pass does not consume alerts: it reports every server due one, on every pass, until the alert is marked delivered. The app posts a notification with Kill and Keep buttons and marks the alert only once Notification Center accepted it. When notifications are not allowed, nothing is marked, the server's row says "notifications off", and the alert goes out once they are allowed.

- **Kill** kills that exact process, asking the core rather than the in-memory list, so it works after an app relaunch. If the server changed since the notification, the app says there is nothing to kill.
- **Keep** marks the server: no more alerts until it restarts (a restart is a new process, so a new row).

`btp watch` prints each alert once and marks it as delivered. `btp sweep` ignores the alert trigger without marking anything, so the app still alerts later.

## 9. Observed servers

The app also lists the servers no agent started. An observed server is any TCP listener of the current user, except a process whose executable is under `/System`, `/usr/libexec` or `/usr/sbin`, or inside an `*.app/Contents/` application bundle (but not a runtime bundle inside a `*.framework`, such as Homebrew's or python.org's `Python.app`, so servers run by framework Python are shown; an executable whose path cannot be read counts as a system one), and except the helper of an application: a listener whose direct parent runs from such an application bundle and whose working directory is `/` or unreadable (an app's bundled node runtime, say), unless that parent is the menu bar app itself, which restarts observed servers as its own children. A server started from a terminal has a shell as parent, and one an IDE's run button starts runs in the project folder: both are shown. Observed listeners are grouped by process tree like tracked servers; grouping stops at an agent CLI or at a process matching an excluded command, so a nested agent session or a database is never folded into a dev server.

Observed servers are never stopped automatically. The row shows `in use`, `idle N min` or `excluded`; an excluded one (command or port, section 6) has no Stop. Stop and Restart exist only in the app, one server at a time, and ask for a confirmation first: Return and Escape both cancel, so a stray key press stops nothing. `btp kill` and `btp sweep` refuse observed servers (an untracked port or pid exits 1), and so does the skill.

## 10. Postpone

An about-to-stop server (a scheduled stop within 10 minutes) offers `Postpone 1 h`: the countdown restarts for one hour from the click. A server whose worktree was deleted still stops at once (trigger 2). Afterwards the dead-session rule applies as usual. The postpone is kept in the state file, and the waiter started by `btp session-end` honors it. Keep keeps its meaning ("stop alerting me") and lives in the more menu.

## 11. Alerts in the app

The menu has a "Needs you" list with one card per server that needs a decision, in this priority: a leak, a server whose session ended but is still open (a browser tab holds it), a stop within 10 minutes, the idle alert. Each card suggests one action (Restart, Postpone or Stop). A kept server shows no stop or idle card (it still shows a leak, or a session that ended but is still open).

**Idle alert.** It re-arms only when the idle value the alert rule uses (section 5, trigger 5) drops below the threshold, so a long-lived connection during an idle session does not cause a new notification on every pass. Its card stays until you choose Ignore this alert or the server is used again.

**Leaks.** A server leaks when its memory (the whole tree) is above `leak_memory_mb` (default 2048), or has grown by more than `leak_growth_mb` (default 500) in the last 10 minutes. The minimums are 100 and 50. Health is green below 500 MB, amber from 500 MB, red when it leaks. A leak raises one notification per leak episode. Ignore this alert dismisses the current alert only; it re-arms when memory grows another 500 MB. The leak state resets after 10 minutes without a leak, so a leak that flaps does not notify twice in a row. The leak state lives in memory: a relaunch of the app can notify an ongoing leak once more. Observed servers can leak too.

**Updates.** The app checks for a new version once a day through Sparkle and, when automatic updates are on (the default), downloads and installs it in the background. This feed is the only network call besides localhost. Updates are signed with an EdDSA key and the app with Developer ID, and the download comes from the GitHub release. The feed is `https://github.com/matthieuh/balance-ton-port/releases/latest/download/appcast.xml` (`SUFeedURL`), a release asset of the latest final release; a prerelease publishes no appcast, so `latest` keeps serving the previous feed. The app is not sandboxed and sends no system profile. Sparkle is pinned to 2.10.0. Settings > About shows the version and build, a Check for Updates button and the Update automatically switch, which moves Sparkle's check and download switches together. Sparkle offers an update only when `CFBundleVersion` is higher, so every final release must raise it.

**Languages.** The app speaks eight languages (English, German, French, Spanish, Simplified Chinese, Hebrew, Japanese, Ukrainian), all copy in one String Catalog. Settings > General > Language picks one or System default (the first macOS preferred language the app supports, else English) and applies at once; Hebrew switches the layout to right to left. The choice is kept in the app's defaults, not in `config.toml`. The CLI and the plugin stay in English. See [LOCALIZATION.md](../LOCALIZATION.md).

## 12. History and probes

**History.** Each pass samples the memory and CPU of every server's whole process tree. Memory is each process's physical footprint, the figure Activity Monitor shows: unlike the resident size it keeps counting pages macOS compressed, so a leak stays visible (the resident size is the fallback when the footprint cannot be read). The samples, and the lifecycle events (started, browser connected or closed, session ended, alert, restarted, postponed), are kept in memory for 2 hours and never written to disk. They feed the growth rule and the chart of the detail view.

**Probes.** To show a title and a favicon, the app fetches the page of a server over http at `127.0.0.1` or `::1`, on the server's own port only. A probe has one deadline of 800 ms. A probe is never counted as use: the app's own connections to a port are ignored when measuring activity (dual-stack aware). A port that answers with something other than HTTP is probed once and never again while the server lives; a timeout or a refusal is retried later. Excluded servers are never probed.

**Preview.** The detail view shows a live preview of the page only for a server that already has a browser connected. It is rendered offscreen in a web view where everything except localhost on that port is blocked: content rules, a script in every frame that removes preconnect and prefetch hints, service workers disabled, and a SOCKS proxy on a dead loopback port for every connection that is not to loopback, so a hint or a font that slips through dies on the machine. A preview is cached for 3 minutes. The connection of the preview is opened by WebKit's networking process, not by the app, so it can count as use for a few seconds. That never changes a decision, because the server was already in use when the preview was taken.

## 13. Restart

Restart stops a server and starts it again: the original argv in its original folder, with the environment read from the live process at the time of the restart (never stored, logged or printed). `argv[0]` is resolved with the process's own `PATH`. A restarted tracked server keeps its ownership: session, note, Keep and Postpone, plus a `restarted_from` record. A pending restart links the new process to its owner while that process lives (24 hours at most), so a child that listens late is still linked. Pending restarts are kept in `state.json`, so they survive a restart of the app or of `btp watch`, and are dropped after 24 hours.

A restart is refused when the folder is gone, the environment cannot be read, the command line cannot be read or its program found, the port is still taken 5 seconds after the stop (nothing is started), an exclusion matches (nothing is stopped or started), another restart or stop of the same server is already running, or the new process could not be started (the server then stays stopped). The server's output goes to `<data dir>/logs/` (mode 0600, because it may hold tokens; the 20 most recent files are kept). Restart is never in the hover capsule; it is in the more menu and on the leak card.

**Session chip.** The detail view's session chip copies `cd '<folder>' && claude --resume <id>` (or `codex resume <id>`) to the clipboard. It never opens a terminal.

## 14. btp run

Watching stays the core. `btp run` is an optional layer that agents use by default and that people can opt into: everything above works without it, and a server started through it keeps running if the app or the watcher quits. It is not a process manager: no daemon, no restart on crash, nothing kept running after `btp run` itself is gone apart from what the child does on its own.

**Command.** `btp run [--free | --reroute | --no-check] [--new] -- <cmd>` runs `<cmd>` as a normal child in the current folder with the current environment. Exit codes: the child's code when it ran, 0 when an existing server was reused, 3 for a port conflict btp did not resolve (nothing is started, and `ask` without a terminal), 2 for a usage error, and 126 or 127 when the command cannot be run or found, like a shell. btp's own lines go to stderr, each starting with `btp: `, so they do not mix with the server's output and an agent can find them.

**Recognition.** btp acts only on a command it recognises as starting a dev server: a serving subcommand of a known tool (`dev`, `start`, `serve`, `preview` and the like, `npm run dev` through the package.json script behind it), never a build command. The tool is named by the program word only: the first word (after `npx`, `bunx` or `pnpx`), or the script `node`, `bun` or `deno` runs (`node node_modules/.bin/next dev`, `node node_modules/vite/bin/vite.js`); a path to `node_modules/.bin/<tool>` counts as `<tool>`. The same goes for the first word of a package.json script. A tool named by an argument is not a dev server: `ls node_modules/.bin/vite`, `git add node_modules/.bin/vite` or `npx rimraf node_modules/.bin/vite` run untouched, and the hook stays silent on them. Any other command, and any command btp cannot understand, runs untouched, exactly as without btp: same output, exit code and signals (it is still logged and linked). Inside a `btp run` (the child has `BTP_RUN_ACTIVE=1`), a nested `btp run` runs its command untouched.

**The port.** The intended port is, in order: a port flag in the command (`--port`, `-p`, `--port=`), the port set by the package.json script behind it, the `PORT` environment variable, the framework's default. `PORT` (inherited, or set inline in a script) counts only for a framework that reads it: Next.js, Nuxt and `remix-serve`; Vite (so SvelteKit and `remix vite:dev`), Astro, wrangler, Expo, Storybook and Eleventy ignore it, so for them it is the default. Every recognised framework has a default. The one exception: a port flag whose value is not a number (`vite --port $PORT` in a script, `--port=$PORT`) leaves the port unknown, and the command runs untouched, as with `--no-check` (no check, no reuse, no rewrite; still logged and linked).

**Reuse.** A server already runs "the same command" when its working directory equals the current one, it is in the same worktree, and it has the same command key: the framework plus the resolved command words with their port arguments removed, so `vite` and `vite --host` differ. Leading `VAR=1` assignments are ignored only inside a package.json script; a direct `cross-env X=1 next dev` is not recognised. A command btp does not recognise is never reused: it runs untouched. Two apps of one monorepo differ by folder and are never confused. `btp run` never reuses itself or one of its own ancestors. `--new` skips reuse.

- For an agent: print the address (`btp: storefront already running on http://localhost:3000 (...), reusing it`) and exit 0. Nothing is started.
- In a terminal: print who runs it and follow its log live if it has one; Ctrl-C stops following and leaves the server running; `r` asks `Stop <tool> on :<port> and start it here? [y/N]`; `y` stops it (the same rules as the app's Stop: an excluded server cannot be stopped) and starts the command here, Return, Escape or any other key keeps it and goes on following. A stop that fails says why (excluded, or btp may not signal it, or a stop or restart of it is under way).

**Agent or person.** btp acts as for an agent (no prompt, no following) when it descends from an agent session, unless both stdin and stderr are terminals, or when it is a background job of its terminal (`btp run ... &` in an interactive shell: not the terminal's foreground process group, so it never prompts or changes the terminal's mode). The prompt and log following exist only in a terminal outside any agent session; in agent mode `ask` exits 3.

**Port taken.** The setting `port_conflict` decides, and the flags override it for one run (`--reroute` beats `--free`; `--no-check` skips the check and reuse). `btp run` reads `config.toml` for this itself; a broken config prints a warning and acts as with the defaults for rerouting, but never frees anything (as `--reroute`):

- `free_idle` (default): if the holder may be freed (below), btp stops it with `kill_tree` (section 7) with the reason `port_conflict`, and starts on that port. Otherwise it reroutes.
- `ask`: in a terminal, btp asks `Port 3000 is used by admin (...). [k]ill it, [o]ther port, [c]ancel?` (kill is offered only for a holder that may be freed). Without a TTY it starts nothing and exits 3 with a message telling the agent to ask the user and rerun with `--free` or `--reroute`.
- `reroute`: never stops anything, always picks another port.

`--free` only ever frees what `free_idle` would free; it never widens what can be stopped.

**What may be freed.** Only while a watcher runs (the app or `btp watch` holds `watch.lock`), because only its passes keep a server's idle clock fresh: without one, the idle time counts from the last pass, however old, so nothing is freed and the port is rerouted around (or the command blocked) instead. Then, only a tracked server whose session is dead and which has been idle for at least `min(10 min, dead_session_idle_s)`, and none of: Keep or `note --keep`, a running Postpone, another live session in its worktree (section 5, trigger 3), an exclusion, in use. An observed server (section 9) or a holder btp does not know is never freed. A live session's server is never freed, idle or not. The checks run again right before the kill, like every kill. Freeing is recorded in the kill history as `port_conflict`.

**Rerouting.** The new port is the first free one above the intended port, at most 100 tries; a port is skipped when any listener holds it (tracked, observed or unknown holders) or it answers on a loopback connect. btp passes it the way the framework reads it (`--port N`, `-p N` or `PORT=N`; after `--` for `npm run`, `pnpm` and `bun`, while `yarn` gets the flag appended directly). Not reroutable, so exit 3 with who holds the port and nothing started: a package.json script that sets the port, for every framework except Next.js, and any script with an inline `PORT=`, Next.js included. btp also exits 3 when it finds no free port. It never starts a command it knows will crash. A command of a framework btp does not know is not recognised and runs untouched.

**Output.** One line per decision, for example `btp: storefront on http://localhost:3001 (3000 is used by admin)`, `btp: freed :3000 (storefront, session ended 40 min ago, idle 25 min)`, `btp: port 3000 is used by postgres. Ask the user, then rerun with --reroute.` (exit 3). A bug of btp before the start never stops the command: it runs as it is, after one line saying the check was skipped.

**Linking.** At start, `btp run` records a pending link keyed on its own process (pid and start time) and the session it descends from. Any listener whose ancestry reaches that process is linked to the session, including a server started with `&` or `nohup` inside it, and including when `btp run` was backgrounded and reparented. No environment is read, so the rule of section 3 is unchanged. A `btp run` outside any agent session links nothing: its servers are observed servers.

**Process and signals.** When stdout is a terminal, the child runs on a pty, so it keeps its colors and its interactive behavior; the terminal then gets the child's stdout and stderr merged, unless stderr is redirected elsewhere. Otherwise it is piped. Each signal (Ctrl-C, SIGTERM, SIGHUP) is forwarded to the child once. A child that dies from a signal makes `btp run` re-raise it. After the child exits, the output already buffered is delivered in full, then `btp run` waits at most 500 ms for the end of the stream: a lingering background process never keeps it from returning.

**Logs.** The output of the child (stdout and stderr, interleaved) is also written to `<data dir>/logs/run/<pid>-<start>.log`, mode 0600 because it may hold tokens. A log is capped at about 5 MB: beyond that the oldest half is dropped and the newest kept. A log is deleted once its server is no longer running and its last write is more than 24 hours old (checked by `btp run`, `btp logs` and the watcher). It is never sent anywhere.

`btp logs <port|name> [-f] [--errors] [-n N]` prints the log of a tracked or observed server started through `btp run` (the log is found through a launcher above the listener, such as `npm` above `node`). `-n` limits to the last N lines (default 200). `-f` follows. `--errors` keeps only the lines that look like errors (`error`, `failed`, `exception`, stack frames, `EADDRINUSE`, framework messages) with 2 lines of context (with `-f`, the matching lines alone, without context). `btp list --json` has `has_logs` per server.

**Check before Claude Code starts a server.** The plugin's PreToolUse hook on the Bash tool (section 16) runs `btp hook pre-bash`, which applies the same port rule without `btp run` and never rewrites the command. It acts only when the command clearly starts a dev server; it stays silent on anything else, including heredocs, comments, subshells and quoted text. It frees a holder only under `port_conflict = free_idle`, when the holder may be freed, and then lets the command run. Under `ask` it denies with the advice plus "Ask the user before freeing it." when the holder could be freed; under `reroute` it never frees. Otherwise it denies the command with a message for the agent (who holds the port, rerun as `btp run -- <cmd>` or use another port; for a command whose port btp cannot change, start it on a free port or ask the user, without suggesting `btp run`). It only frees, and never denies, when `BTP_SHELL_WRAP=1` is set, because the wrapper will reroute the command (see the shell setup below). On any internal error, an unreadable config or `port_check_hook = false`, it exits 0 and prints nothing: it never blocks the agent because of btp. When it frees a port it waits for the kill (up to `kill_grace_s`); the hook timeout covers that.

**Shell setup (opt-in).** `btp shell-init zsh|bash` prints shell functions; the user adds `eval "$(btp shell-init zsh)"` to their rc file. btp never edits an rc file. The functions wrap `npm`, `pnpm`, `yarn`, `bun`, `npx` and the bare framework commands (`next`, `vite`, `astro`, `wrangler`, `expo`...), call `btp run` only when the command starts a known dev server, and send everything else straight to the real command. A user alias wins. A user function defined before the eval line is replaced by the wrapper; one defined after it wins. `BTP_RUN=0` turns the wrapping off for one command. The snippet exports `BTP_SHELL_WRAP=1` into the shell, so a Claude Code started from a wrapped shell has a hook that only frees and never blocks: its dev commands are rerouted by the wrapper.

**Not covered.** A server that daemonizes itself away from `btp run` is not linked. See section 17 for the other gaps.

## 15. State, config and locking

Everything lives in `~/Library/Application Support/balance-ton-port/` (overridable with `BTP_DATA_DIR`):

| File | Content |
|---|---|
| `config.toml` | Settings, shared by the app and the CLI |
| `state.json` | Tracked servers (links, idle clocks, Keep, Postpone and alerted marks) and the last 5 kills |
| `state.lock` | Exclusive lock held while `state.json` is read and rewritten |
| `kills.log` | One JSON line per kill |
| `logs/` | Output of restarted servers, one file per restart (mode 0600, the 20 most recent kept) |
| `logs/run/` | Output of servers started through `btp run`, `<pid>-<start>.log` (mode 0600, about 5 MB each, deleted 24 h after the server stopped) |
| `watch.lock` | Held by the single watcher, with its name inside |

`state.json` is written through a temp file and a rename. A corrupt file is moved to `state.json.corrupt` and the state starts empty; any other read error is returned so the state is never overwritten blindly.

Besides the numeric keys, `port_conflict` (`free_idle`, `ask` or `reroute`, default `free_idle`) and `port_check_hook` (`true` or `false`, default `true`) belong to section 14; the app's Settings has a "Port conflicts" choice and a switch for them.

Config minimums: 60 s for `dead_session_idle_s` and `alert_after_s`, 1 s for `kill_grace_s` and `scan_interval_s`. `btp config set` and Settings refuse lower values; values read from the file are raised to the minimum with a note on stderr, printed once per change of the file. Saves are atomic, and the last writer wins. A file that does not parse never silently becomes the defaults: a running watcher keeps its last valid config (a fresh start uses the defaults), the app shows the error, and `btp config` refuses to read or write until it is fixed.

**One watcher.** Only the holder of `watch.lock` runs killing passes. The app tries to take it on every pass; while `btp watch` holds it, the app only observes (links, activity and forgets are still recorded, no kill, no alert) and shows the list read-only, but its manual Kill button still works. `btp watch` refuses to start while the app holds the lock. `btp list`, `btp kill` and `btp sweep` work without any watcher: they run the same pass (link, observe, forget) on demand.

## 16. Claude Code plugin and session end

The plugin (`plugin/`) has three hooks (PreToolUse, SessionStart, SessionEnd), an install command and the agent skill.

**PreToolUse.** `hooks/pre-bash.sh` runs `btp hook pre-bash` on every Bash tool call, as described in section 14: it frees a freeable holder of a dev server's port or denies the command with advice, and stays silent on everything else. `port_check_hook = false` turns it off. Installing the plugin is the opt-in; btp edits no Claude Code settings.

**Session end never stops anything.** The SessionEnd hook runs `btp session-end` in a detached child (Claude Code gives the hook about 1.5 s and shows nothing it prints, so the hook prints nothing and always exits 0). The session process is the hook's parent. With "Exit and stop tasks", Claude Code stops its own background tasks before the hook runs, so the hook finds no server. With "Move to background and exit", the servers are still alive: a server alive at that moment is one the user chose to keep. `btp session-end` records them as one ended-session group, with the process identity of the session so the link survives, and posts one notification ("Session ended: N servers kept running: ... Stopped after 20 min unused, once no session works in that folder"). "Move to background" leaves a live background session in the same folder, so its servers are kept while it runs. Nothing is posted when no server was kept. The app posts it on its next pass when it holds the watch lock; otherwise `btp session-end` posts it with `osascript` and marks the group delivered under the state lock, so it is never shown twice. `clear` and `resume` reasons do nothing: the process lives on.

**Who stops the kept servers.** They follow the normal dead-session rule: idle for `dead_session_idle_s`, with every protection (exclusions, Keep, shared worktree). The app or `btp watch` applies it. When neither runs, `btp session-end` starts a detached waiter (`btp _sweep-later`, a double fork) that sleeps until the first server is due, runs the same pass restricted to that session's servers with a fresh config, and repeats until none is left. If a watcher holds the lock when it wakes, it does nothing: the watcher applies the rule. It gives up after 24 hours.

**Notes.** The agent tells `btp` its intent through the skill. `btp note <port> --keep "<why>"` has the effect of Keep (no alerts, not offered after `/clear`) and stores the reason, which phrases the `/clear` question; the app and `btp list` do not show it yet (a later version will). `btp note <port> --done` makes the server eligible as soon as it is idle once its session has ended, without waiting `dead_session_idle_s`. A note only makes things more cautious or speeds up a kill the rules already allow: it never stops a server that is in use, excluded, shared with another live session, or of another session (the CLI checks that the caller descends from the session's process). Notes are cleared when the server restarts.

**`/clear` and `/resume`.** The SessionStart hook runs `btp stale --conversation <id>`: the servers recorded under an earlier conversation id of the same live process, idle more than 5 minutes, each listed once. Claude asks the user a single question in the new conversation, using the notes to describe them, and a yes runs `btp kill --key <pid>:<start time>` for each: a hidden form that stops exactly the process offered, only while it is still a tracked server of the caller's own session, so whatever took the port since is never touched. Kept and excluded servers are not offered.

**Install.** Another SessionStart check finds `btp` (and a minimum version). If it is missing or too old, Claude asks before installing. The script downloads the latest release, checks the sha256, verifies the Developer ID signature with `codesign`, and installs to `~/.local/bin/btp`, never with sudo and never over a `btp` the plugin did not install. Two markers live under the plugin's data dir. The user's "no" (`install-declined`) is cleared by `/balance-ton-port:install` or a successful install. A refused install (`install-refused`, holding the tag of a release older than the plugin needs) keeps the question away until a newer release tag exists (looked up at most once a day), and is cleared by a successful install.

**Debugging.** With `BTP_HOOK_DEBUG=1`, the hook and `btp session-end` append one line per step to `session-end.log` in the data dir (what was found, why nothing was done). It is off by default.

**Known gaps.**
- No hook runs when Claude Code crashes or is killed hard, so nothing is recorded; the normal rules take over.
- A server reparented to launchd before the hook runs is left to the normal rules.
- A server started with `&` or `nohup` outside `btp run` is reparented at once, so it is never linked to the session and never tracked. Started through `btp run` (section 14), it is linked.

## 17. Known limitations

- A Claude Code process with no session file in a watched dir (a `CLAUDE_CONFIG_DIR` whose `sessions/` folder is shared with another dir, an older Claude Code) is ignored: its servers are never tracked, so never killed, and never cleaned up either.
- A server started in the background (`cmd &`, `nohup`) inside one shell command, not through `btp run`, is reparented to launchd before the first scan and is not tracked, for Codex as for Claude Code. More generally, anything started and orphaned between two scans is missed. Through `btp run` it is linked (section 14), except a server that daemonizes itself away from `btp run`.
- A port set in a package.json script is not reroutable (exit 3) for any framework except Next.js, and an inline `PORT=` in a script never is, Next.js included.
- In a terminal, `btp run` merges the child's stdout and stderr (pty) unless stderr is redirected elsewhere.
- The port check applies to commands btp recognises as starting a dev server; any other command runs untouched. So does a recognised command whose port flag btp cannot read (`--port $PORT` in a script).
- With no watcher running (neither the app nor `btp watch`), `btp run` and the hook never free a port: they reroute around the holder, or block when the port cannot change.
- Codex desktop app is not supported: its `codex` processes are never sessions, so its servers are never tracked.
- A live Codex session's servers are alerted on port idle only (its session idle time is unknown, see section 2), like a busy Claude Code session's; session-end cleanup, the offer after `/clear` and the grouped notification are Claude Code only.
- A Codex session without a lock or a rollout yet has an empty conversation id.
- The native Codex binary must be named `codex`; a vendor-triple name is not recognised.
- A resumed Codex thread keeps the cwd recorded in its transcript, not the one it was resumed from.
- Inside the default Codex sandbox a server cannot listen on a port and btp's data dir is read-only (see the agent snippet `plugin/skills/balance-ton-port/AGENTS.md`).
- Command exclusions are substrings of the whole command line, paths included: a project path containing `mcp` excludes its servers. That errs on the safe side.
- Port activity is sampled once per pass. A short request that opens and closes between two passes is not seen, so a server used only by brief, rare requests can look idle.
- The shared-worktree rule compares worktree roots only (see section 5).
- A `btp watch` older than 0.3 ignores Postpone (it still stops on the usual countdown) and, when it rewrites `state.json`, drops the state 0.3 adds (postpones, pending restarts, `restarted_from` records, session folders). Run one version of `btp` and the app.
- Mixing 0.4 and 0.5: a 0.4 app that saves Settings drops `port_conflict` and `port_check_hook` from `config.toml`, so they go back to their defaults (`free_idle`, hook on). A 0.4 watcher that rewrites `state.json` drops `pending_runs`: the link is lost, so a server started through `btp run` becomes an observed one, which is never killed by it.
- The kill log grows without bound, and the same kill can occasionally be recorded twice.
- macOS only (the Homebrew formula too). The platform layer is behind a trait so that a Linux implementation (`/proc`) can follow.

## 18. Architecture

```
crates/btp-core   Rust library: platform layer, sessions, linking, decide, kill, state, config
crates/btp-cli    the `btp` binary (clap): `run`, `logs`, `hook`, `shell_init` for section 14
crates/btp-ffi    UniFFI wrapper around the core for Swift
app/              SwiftUI menu bar app (XcodeGen project)
plugin/           the Claude Code plugin: hooks, install script, the agent skill (plugin/skills/)
```

- `btp-core`: `platform` (a `Platform` trait; the macOS implementation uses libproc and sysctl for listening sockets, process info, ancestry, descendants and per-socket TCP states, plus `kill(2)`), `sessions` (session files and config dir discovery), `agents` (one module per agent: process recognition, live sessions and metadata for `claude` and `codex`, the rollout line 1 reader), `worktree`, `decide` (pure decision logic, unit-tested with fake clocks and fake process tables), `watcher` (link, refresh, observe, execute, the watch lock; `watcher/for_run` for the port holder, free-for-run and reuse queries and the run links), `run` (`command`: recognition of dev commands and port rewriting; `conflict`: the port conflict decision), `runlog` (run logs, size cap, cleanup, error filter), `history` (samples, events, leaks), `observed`, `probe` (title and favicon), `restart`, `state`, `config`, and `labels`/`display` for readable names (`Expo (Metro)`, `repo › worktree`).
- `btp-ffi`: a `Core` object exposing `tick`, `observe`, `kill_port`, `kill_server(pid, start_time)`, `mark_alerted`, `keep`, `postpone`, `ignore_alert`, `mark_leak_notified`, `history`, `events`, `icon`, `processes`, `resume_command`, `restart`, `stop_observed`, `discovered_claude_dirs`, `config`/`set_config`, `claim_session_ended`, `unclaim_session_ended`, `stop_session_ended`, `keep_session_ended`, `recent_kills` and `acquire_watch`. `scripts/build-xcframework.sh` builds a universal static library, packs it as `BtpCore.xcframework` and generates the Swift bindings.
- The app: `MenuBarExtra` in window style, `LSUIElement` (no Dock icon), launch at login through `SMAppService`, notifications with actions through `UNUserNotificationCenter`. It runs one pass at a time on a background queue and schedules the next one `scan_interval_s` after the previous one finished. One process, no daemon to install. macOS 14 or later.

## 19. Release pipeline

CI runs fmt, clippy, the Rust tests and the Swift tests on every push and pull request. A `v*` tag runs the release workflow: it checks that the tag matches the CLI and app versions, builds a universal `btp` and app, signs them with a Developer ID, notarizes and staples the app, packs it into `BalanceTonPort-<v>.dmg` (signed, notarized and stapled too), and publishes a GitHub release with the CLI tarball, the dmg, their `.sha256` files and the Homebrew formula and cask filled with the version and checksums; a tag with a hyphen is a prerelease, and nothing is published unless every step passed.

For a final release the workflow also signs the dmg with the Sparkle EdDSA key (`SPARKLE_ED_PRIVATE_KEY`) and attaches `appcast.xml`, after checking that `CFBundleVersion` is above the one in the published feed. A separate `tap` job (after `release`) then commits the filled formula (`btp`, macOS only for now) and cask (`balance-ton-port`) to `matthieuh/homebrew-tap` with `HOMEBREW_TAP_TOKEN`; it can be rerun alone. A prerelease skips both the appcast and the tap. See [RELEASING.md](../RELEASING.md) for the secrets and the steps.
