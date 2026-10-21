# Balance ton port v1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A macOS menu bar app and a `btp` CLI that kill dev servers left behind by Claude Code sessions (worktree deleted, or session dead and port idle) and alert on idle servers of live sessions.

**Architecture:** A Rust workspace: `btp-core` (pure decision logic + macOS platform layer + state/config), `btp` (CLI), `btp-ffi` (UniFFI bindings). A SwiftUI `MenuBarExtra` app links `btp-ffi` as an xcframework and drives `Watcher::tick` on a timer. Detection is a scan every `scan_interval` (default 5 s) through libproc; no subprocess, no `lsof`.

**Tech Stack:** Rust stable (edition 2021), `libproc` crate, `libc`, `serde`/`serde_json`/`toml`, `fs4` (file lock), `clap` 4, `uniffi` 0.28; Swift 6 / SwiftUI, macOS 14+, `UserNotifications`, `ServiceManagement`.

**Spec:** `docs/superpowers/specs/2026-10-21-balance-ton-port-design.md`

## Global Constraints

- Never kill a process that was not linked to a live Claude Code session when first seen.
- Defaults: `dead_session_idle` 20 min, `alert_after` 30 min, `kill_grace` 5 s, `scan_interval` 5 s; all configurable in `~/Library/Application Support/balance-ton-port/config.toml` (shared by app and CLI).
- State and kill log live in the same directory (`state.json`, `kills.log`), every read-modify-write under an exclusive file lock on `state.lock`.
- Default exclusions (substring of the command line, case-insensitive): `mcp`, `argent`, `postgres`, `redis-server`, `mysqld`, `mongod`, plus any listener whose pid is itself a Claude Code process.
- A server whose worktree is also the worktree of another **live** Claude session is never killed or alerted on.
- Claude Code sessions are read from `<dir>/sessions/<pid>.json` for each dir in `claude_config_dirs` (default: `~/.claude`, plus `$CLAUDE_CONFIG_DIR` when set). A session is alive when its pid exists and the process start time matches (`procStart`/pid-reuse guard).
- Kill = SIGTERM to the server and its descendants, SIGKILL after `kill_grace` to survivors.
- Menu bar only (`LSUIElement`), notification buttons exactly `Kill` and `Keep`; "Recently killed" shows the last 5.
- Copy never uses the em dash.
- Plan decision: the scan is the only detector (process death, worktree deletion, new listeners). The spec's "event-driven where the OS allows" is met by cost, not mechanism: delays are minutes, a 5 s scan of libproc costs milliseconds; kqueue/FSEvents can be added later without changing `decide`.

## Review Focus

1. **Pid reuse**: a tracked server's pid now belongs to another process → it must be forgotten, never killed (start time mismatch). Test in Task 2.
2. **Session resumed in the same worktree** (old session dead, new live session same worktree) → no kill. Test in Task 2.
3. **Corrupt or missing `state.json` / `config.toml`** → start fresh with defaults and log, never crash. Test in Task 3.
4. **Server restarted on the same port** (new pid) after Keep → Keep no longer applies and it is tracked anew. Test in Task 2.
5. **`btp watch` while the app watches** → refuses with a clear message. Test in Task 6.

---

### Task 1: Toolchain, workspace and config

**Files:**
- Create: `Cargo.toml` (workspace: `crates/btp-core`, `crates/btp-cli`, `crates/btp-ffi`), `crates/btp-core/Cargo.toml`, `crates/btp-core/src/lib.rs`, `crates/btp-core/src/config.rs`, `rust-toolchain.toml` (stable), `.gitignore`, `LICENSE` (MIT, "Matthieu Hostache")

**Interfaces:**
- Produces:
  - `pub struct Config { pub dead_session_idle_s: u64, pub alert_after_s: u64, pub kill_grace_s: u64, pub scan_interval_s: u64, pub claude_config_dirs: Vec<PathBuf>, pub exclude_commands: Vec<String> }` with `Default` (values above) and serde (TOML keys = field names).
  - `pub fn data_dir() -> PathBuf` (`~/Library/Application Support/balance-ton-port`, overridable by env `BTP_DATA_DIR` for tests).
  - `pub fn load_config(dir: &Path) -> Config` (missing or invalid file → defaults, invalid logged to stderr), `pub fn save_config(dir: &Path, c: &Config) -> io::Result<()>`.
  - `impl Config { pub fn set(&mut self, key: &str, value: &str) -> Result<(), String> }` for the CLI (`dead_session_idle_s`, `alert_after_s`, `kill_grace_s`, `scan_interval_s` accept integers; unknown key → Err).

- [ ] **Step 1:** Install the toolchain if missing: `brew install rustup && rustup-init -y --default-toolchain stable` (ask the user before installing). Verify `cargo --version`.
- [ ] **Step 2: Failing tests** in `config.rs`: `defaults_match_spec` (1200, 1800, 5, 5, exclusions list exact), `missing_file_gives_defaults`, `invalid_toml_gives_defaults`, `partial_file_keeps_other_defaults` (file with only `alert_after_s = 60`), `set_rejects_unknown_key_and_non_integer`, `save_then_load_roundtrips`.
- [ ] **Step 3:** `cargo test -p btp-core` → FAIL.
- [ ] **Step 4:** Implement (`#[serde(default)]` on the struct).
- [ ] **Step 5:** `cargo test -p btp-core` → PASS; `cargo clippy -- -D warnings` clean.
- [ ] **Step 6:** Commit `feat(core): workspace and config`.

### Task 2: Decision engine (pure)

**Files:**
- Create: `crates/btp-core/src/model.rs`, `crates/btp-core/src/decide.rs`

**Interfaces:**
- Consumes: `Config`.
- Produces:
  - `pub struct ServerKey { pub pid: i32, pub start_time: u64 }` (Eq, Hash, serde)
  - `pub struct TrackedServer { pub key: ServerKey, pub port: u16, pub command: String, pub worktree: PathBuf, pub session_id: String, pub session_pid: i32, pub first_seen_ms: u64, pub last_active_ms: u64, pub kept: bool, pub alerted: bool }`
  - `pub struct Observation { pub key: ServerKey, pub alive: bool, pub established: u32, pub worktree_exists: bool, pub session_alive: bool, pub session_idle_ms: Option<u64>, pub shared_with_live_session: bool }`
  - `pub enum KillReason { WorktreeDeleted, DeadSessionIdle { idle_ms: u64 }, Manual }`
  - `pub enum Action { Kill { key: ServerKey, reason: KillReason }, Alert { key: ServerKey, idle_ms: u64 }, Forget { key: ServerKey } }`
  - `pub fn decide(servers: &[TrackedServer], obs: &[Observation], now_ms: u64, cfg: &Config) -> Vec<Action>`
  - `pub fn touch(server: &mut TrackedServer, o: &Observation, now_ms: u64)` (sets `last_active_ms = now_ms` when `o.established > 0`)

Rules, in order per server: not alive (or no observation) → `Forget`; `!worktree_exists` → `Kill(WorktreeDeleted)`; `shared_with_live_session` → nothing; `!session_alive` and port idle ≥ `dead_session_idle_s` → `Kill(DeadSessionIdle)`; `session_alive` and not `kept` and not `alerted` and max(port idle, session idle) ≥ `alert_after_s` → `Alert`. Port idle = `now_ms - last_active_ms`.

- [ ] **Step 1: Failing tests** in `decide.rs` (fake times, minutes as ms): `dead_process_is_forgotten_not_killed`; `pid_reused_is_forgotten` (observation absent for the key → Forget); `worktree_deleted_kills_immediately_even_if_session_alive`; `dead_session_kills_after_20_min_port_idle` (19 min → nothing, 20 min → Kill); `dead_session_active_port_is_kept` (established > 0 keeps resetting via `touch`); `resumed_session_same_worktree_never_killed` (shared → no action even at 2 h); `live_session_idle_30_min_alerts_once` (second call with `alerted = true` → nothing); `session_idle_alone_triggers_alert` (port active, `session_idle_ms` 31 min → Alert); `kept_server_never_alerts`; `new_pid_after_keep_is_a_new_server` (different `ServerKey` with `kept = false` → alerts); `delays_follow_config` (alert_after_s = 60 → alert at 1 min).
- [ ] **Step 2:** `cargo test -p btp-core decide` → FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** PASS, clippy clean.
- [ ] **Step 5:** Commit `feat(core): decision engine`.

### Task 3: State store and kill log

**Files:**
- Create: `crates/btp-core/src/state.rs`

**Interfaces:**
- Consumes: `TrackedServer`, `KillReason`, `ServerKey`.
- Produces:
  - `pub struct KillRecord { pub port: u16, pub command: String, pub worktree: PathBuf, pub reason: KillReason, pub at_ms: u64 }`
  - `pub struct State { pub servers: Vec<TrackedServer>, pub recent_kills: Vec<KillRecord> }` (serde; `recent_kills` capped at 5, newest first)
  - `pub fn with_state<T>(dir: &Path, f: impl FnOnce(&mut State) -> T) -> io::Result<T>`: exclusive lock on `state.lock`, load (corrupt/missing → empty, original renamed `state.json.corrupt`), run `f`, write atomically (temp + rename).
  - `pub fn append_kill_log(dir: &Path, r: &KillRecord) -> io::Result<()>` (one JSON line per kill in `kills.log`).
  - `impl State { pub fn record_kill(&mut self, r: KillRecord) }`

- [ ] **Step 1: Failing tests** (temp dirs): `missing_state_is_empty`, `corrupt_state_starts_fresh_and_keeps_copy`, `recent_kills_capped_at_5_newest_first`, `with_state_persists_changes`, `concurrent_with_state_serializes` (two threads each push 100 servers → 200), `kill_log_appends_lines`.
- [ ] **Step 2:** FAIL. **Step 3:** implement (`fs4` exclusive lock). **Step 4:** PASS. **Step 5:** Commit `feat(core): state store and kill log`.

### Task 4: Claude sessions reader

**Files:**
- Create: `crates/btp-core/src/sessions.rs`, `crates/btp-core/tests/fixtures/sessions/*.json`

**Interfaces:**
- Produces:
  - `pub enum SessionStatus { Idle, Busy, Unknown }`
  - `pub struct ClaudeSession { pub pid: i32, pub session_id: String, pub cwd: PathBuf, pub status: SessionStatus, pub status_updated_at_ms: u64, pub proc_start: String }`
  - `pub fn read_sessions(dirs: &[PathBuf]) -> Vec<ClaudeSession>` (reads `<dir>/sessions/<pid>.json`; ignores `.key` files and unparsable files; JSON keys `pid`, `sessionId`, `cwd`, `status`, `statusUpdatedAt`, `procStart`; unknown `status` → Unknown)
  - `pub fn session_idle_ms(s: &ClaudeSession, now_ms: u64) -> Option<u64>` (Some(now − statusUpdatedAt) only when `Idle`)
  - `pub fn default_config_dirs() -> Vec<PathBuf>` (`~/.claude`, plus `$CLAUDE_CONFIG_DIR` expanded, `~` expanded, deduplicated, only existing)

Example file (real shape, Claude Code 2.1.300): `{"pid":12345,"sessionId":"00000000-…","cwd":"/Users/me/my-app","startedAt":1792068700750,"procStart":"Thu Oct 15 12:51:40 2026","kind":"interactive","status":"idle","statusUpdatedAt":1792574891213}`.

- [ ] **Step 1: Failing tests**: `parses_real_session_file`, `skips_key_and_garbage_files`, `idle_ms_only_when_idle`, `tilde_config_dir_is_expanded` (`CLAUDE_CONFIG_DIR="~/.claude-perso"` must resolve, a real-world trap), `dirs_deduplicated`.
- [ ] **Step 2:** FAIL. **Step 3:** implement. **Step 4:** PASS. **Step 5:** Commit `feat(core): read Claude Code sessions`.

### Task 5: macOS platform layer

**Files:**
- Create: `crates/btp-core/src/platform/mod.rs`, `crates/btp-core/src/platform/macos.rs`, `crates/btp-core/tests/platform_macos.rs`

**Interfaces:**
- Produces:
  - `pub struct Listener { pub pid: i32, pub port: u16 }`
  - `pub struct ProcInfo { pub pid: i32, pub ppid: i32, pub start_time: u64, pub cwd: Option<PathBuf>, pub command: String }`
  - `pub trait Platform { fn listeners(&self) -> Vec<Listener>; fn proc_info(&self, pid: i32) -> Option<ProcInfo>; fn established(&self, pid: i32, port: u16) -> u32; fn descendants(&self, pid: i32) -> Vec<i32>; fn signal(&self, pid: i32, sig: i32) -> bool; fn proc_start_string(&self, pid: i32) -> Option<String>; }`
  - `pub struct MacOs;` implementing it with `libproc` (`listpids`, `pidinfo` BSDInfo, `pidfdinfo` socket info for TCP state LISTEN/ESTABLISHED and local port, `pidcwd`/vnode path) and `libc::kill`. `proc_start_string` formats the start time like Claude's `procStart` (`%a %b %e %H:%M:%S %Y`, local time) for the liveness check.
  - `pub fn ancestry(p: &dyn Platform, pid: i32) -> Vec<ProcInfo>` (pid up to 1, cycle-safe, max 64)
  - A `FakePlatform` (in `platform/mod.rs`, `#[cfg(any(test, feature = "fake"))]`) backed by maps, used by Task 6 tests.

- [ ] **Step 1: Failing integration tests** (macOS only): `finds_spawned_listener` (spawn `python3 -m http.server 0`-style child via a tiny Rust `TcpListener` helper binary in `tests/`, assert its pid and port appear), `ancestry_reaches_test_process`, `established_counts_a_connection` (connect a client, count ≥ 1), `signal_terminates_child`, `proc_start_matches_ps` (compare with `ps -o lstart= -p`).
- [ ] **Step 2:** FAIL. **Step 3:** implement. **Step 4:** PASS. **Step 5:** Commit `feat(core): macOS platform layer`.

### Task 6: Watcher (linking, tick, kill executor)

**Files:**
- Create: `crates/btp-core/src/watcher.rs`, `crates/btp-core/src/worktree.rs`

**Interfaces:**
- Consumes: everything above.
- Produces:
  - `pub fn worktree_root(cwd: &Path) -> PathBuf` (nearest ancestor containing `.git` file or dir; else `cwd`)
  - `pub fn link(listener: &Listener, chain: &[ProcInfo], sessions: &[ClaudeSession], cfg: &Config, now_ms: u64) -> Option<TrackedServer>` (first ancestor whose pid is a session pid; None if excluded command, if the listener itself is a session pid, or no session in the chain; `worktree = worktree_root(server cwd)`)
  - `pub struct TickReport { pub servers: Vec<ServerView>, pub alerts: Vec<ServerView>, pub killed: Vec<KillRecord> }`
  - `pub struct ServerView { pub key: ServerKey, pub port: u16, pub command: String, pub worktree_name: String, pub session_alive: bool, pub idle_ms: u64, pub kill_in_ms: Option<u64> }`
  - `pub struct Watcher<P: Platform> { … }` with `pub fn new(platform: P, data_dir: PathBuf) -> Self`, `pub fn tick(&mut self, now_ms: u64) -> io::Result<TickReport>` (reload config, read sessions + liveness, link new listeners, observe, `touch`, `decide`, execute: Kill → `kill_tree` + log + `record_kill`; Alert → set `alerted` and return; Forget → drop), `pub fn observe(&mut self, now_ms: u64) -> io::Result<TickReport>` (same as `tick` but executes no kill and sets no `alerted`; `killed` lists what `tick` would kill), `pub fn kill(&mut self, port_or_pid: u32) -> io::Result<Option<KillRecord>>` (Manual), `pub fn keep(&mut self, key: ServerKey) -> io::Result<()>`.
  - `pub fn kill_tree(p: &dyn Platform, pid: i32, grace: Duration)` (SIGTERM pid + descendants, wait up to grace polling every 100 ms, SIGKILL survivors).
  - Watch ownership: `pub fn acquire_watch_lock(dir: &Path, owner: &str) -> io::Result<WatchLock>` (exclusive lock on `watch.lock`, file content = owner; second acquirer gets `Err` whose message names the owner).

- [ ] **Step 1: Failing tests** with `FakePlatform` and a temp data dir: `links_server_under_claude_session`, `ignores_server_without_claude_ancestor`, `excluded_command_not_linked`, `worktree_root_finds_git_file_of_linked_worktree`, `tick_kills_after_dead_session_idle_and_logs` (advance fake clock 20 min), `tick_alerts_once`, `shared_worktree_not_killed`, `kill_tree_escalates_to_sigkill` (fake ignores SIGTERM), `manual_kill_by_port`, `second_watch_lock_names_owner`.
- [ ] **Step 2:** FAIL. **Step 3:** implement. **Step 4:** PASS. **Step 5:** Commit `feat(core): watcher`.

### Task 7: `btp` CLI

**Files:**
- Create: `crates/btp-cli/Cargo.toml` (bin name `btp`), `crates/btp-cli/src/main.rs`, `crates/btp-cli/tests/cli.rs`

**Interfaces:**
- Consumes: `Watcher<MacOs>`, `load_config`, `save_config`, `Config::set`, `acquire_watch_lock`.
- Commands:
  - `btp list [--json]`: `Watcher::observe`, prints `PORT  COMMAND  WORKTREE  STATE  IDLE`.
  - `btp kill <port|pid>`: manual kill, prints the record; exit 1 if nothing matched.
  - `btp sweep [--dry-run]`: one `tick` (`--dry-run` uses `observe`); triggers 1 and 2 only (alerts are dropped); `--dry-run` prints planned kills and executes nothing.
  - `btp watch`: acquires the watch lock as `"btp watch"`, loops `tick` every `scan_interval_s`, prints kills and alerts; if the lock is held prints `Already watched by <owner>` and exits 1.
  - `btp config [get|set <key> <value>]`.

- [ ] **Step 1: Failing tests** (`assert_cmd`, `BTP_DATA_DIR` temp): `config_set_then_get`, `config_set_unknown_key_fails`, `sweep_dry_run_kills_nothing` (spawned listener child stays alive), `kill_unknown_port_exits_1`, `watch_refuses_when_locked` (hold the lock in-test with owner "Balance ton port.app").
- [ ] **Step 2:** FAIL. **Step 3:** implement with `clap` derive. **Step 4:** PASS. **Step 5:** Commit `feat(cli): btp`.

### Task 8: UniFFI bindings and xcframework

**Files:**
- Create: `crates/btp-ffi/Cargo.toml` (`crate-type = ["staticlib", "lib"]`), `crates/btp-ffi/src/lib.rs`, `crates/btp-ffi/uniffi-bindgen.rs`, `scripts/build-xcframework.sh`

**Interfaces:**
- Produces (UniFFI object `Core`): `constructor new()` (data_dir default), `tick() -> TickReportFfi`, `kill(port_or_pid: u32) -> Option<KillRecordFfi>`, `keep(pid: i32, start_time: u64)`, `config() -> ConfigFfi`, `set_config(c: ConfigFfi)`, `recent_kills() -> Vec<KillRecordFfi>`, `acquire_watch() -> bool` (owner "Balance ton port.app"). FFI records mirror `ServerView`/`KillRecord`/`Config` with `reason` as a display string ("worktree deleted", "dead session + idle 23 min", "manual"). `Core` holds the `Watcher<MacOs>` behind a `Mutex`.
- `scripts/build-xcframework.sh`: builds `aarch64-apple-darwin` and `x86_64-apple-darwin` release staticlibs, `lipo` universal, generates Swift bindings, outputs `app/Frameworks/BtpCore.xcframework` and `app/Sources/Generated/btp_ffi.swift`.

- [ ] **Step 1:** Failing Rust test `ffi_reason_strings` (exact strings above).
- [ ] **Step 2–4:** implement, PASS; run the script → xcframework exists, generated Swift compiles with `swiftc -parse`.
- [ ] **Step 5:** Commit `feat(ffi): UniFFI bindings and xcframework build`.

### Task 9: Menu bar app

**Files:**
- Create: `app/BalanceTonPort.xcodeproj` (generated from `app/project.yml` with XcodeGen, `brew install xcodegen`), `app/Sources/BalanceTonPortApp.swift`, `app/Sources/CoreModel.swift`, `app/Sources/MenuView.swift`, `app/Info.plist` (`LSUIElement = YES`, bundle id `com.matthieuh.balancetonport`), `app/Assets.xcassets` (template menu bar icon, anchor glyph)

**Interfaces:**
- Consumes: `Core` (Task 8).
- `CoreModel: ObservableObject` (`@MainActor`): `servers: [ServerViewFfi]`, `recentKills: [KillRecordFfi]`; starts `Core.acquire_watch()`, then a `Timer` every `scan_interval_s` calling `tick()` off the main thread; exposes `kill(_:)`.
- `MenuBarExtra("Balance ton port", image: "MenuIcon") { MenuView() }.menuBarExtraStyle(.window)`; layout exactly the spec's menu: rows `● port command worktree state [Kill]` (`○` + "session dead · kill in N min" when `kill_in_ms` is set), divider, "Recently killed" (5 rows: port, command, reason, relative time), divider, `Settings…`, `Quit`. Empty state: "No dev servers from Claude Code sessions".

- [ ] **Step 1:** Build: `xcodebuild -project app/BalanceTonPort.xcodeproj -scheme BalanceTonPort build` → succeeds.
- [ ] **Step 2:** Manual check (document in the task report): start a Claude session that runs `python3 -m http.server 8765`, the row appears within 5 s; [Kill] removes it and adds a "Recently killed" row; no Dock icon.
- [ ] **Step 3:** Commit `feat(app): menu bar app`.

### Task 10: Notifications, Settings, launch at login

**Files:**
- Create: `app/Sources/Notifier.swift`, `app/Sources/SettingsView.swift`
- Modify: `app/Sources/CoreModel.swift`, `app/Sources/BalanceTonPortApp.swift`

**Interfaces:**
- `Notifier`: requests authorization at first launch; category `IDLE_SERVER` with actions `Kill` (destructive) and `Keep`; title "Balance ton port", body "`<command>` on port <port> (worktree <name>) hasn't been used for <N> min."; `userInfo` carries pid + start_time; Kill → `Core.kill(pid)`, Keep → `Core.keep(pid, start_time)`.
- `SettingsView` (SwiftUI `Settings` scene): four steppers in minutes/seconds for the four delays, a list editor for `claude_config_dirs` and `exclude_commands`, a "Launch at login" toggle (`SMAppService.mainApp`); writes through `Core.set_config`.

- [ ] **Step 1:** Build succeeds.
- [ ] **Step 2:** Manual check: set `alert_after` to 1 min, leave a session idle, the notification shows with Kill/Keep, both work; changing a delay is visible in `btp config get`.
- [ ] **Step 3:** Commit `feat(app): notifications and settings`.

### Task 11: Agent skill and README

**Files:**
- Create: `skills/balance-ton-port/SKILL.md`, `README.md`

`SKILL.md` frontmatter `name: balance-ton-port`, description "Use before starting or after finishing a local dev server, or when a port is already in use". Body rules: run `btp list --json` before starting a server; same worktree already serving → reuse its port; leftover (session dead) → `btp kill <port>`; another live session's server → never touch, pick another port; end of task → `btp sweep`; never `lsof | xargs kill`. README: what it does (the three triggers), install (Homebrew lines from Task 12), CLI usage, config keys and defaults, skill install (`cp -r skills/balance-ton-port ~/.claude/skills/`), safety rules, MIT.

- [ ] **Step 1:** Write both; every command in them runs as written against the Task 7 binary.
- [ ] **Step 2:** Commit `docs: agent skill and README`.

### Task 12: Release pipeline and Homebrew

**Files:**
- Create: `.github/workflows/ci.yml` (fmt, clippy, `cargo test` on `macos-14`), `.github/workflows/release.yml` (on tag `v*`: build universal `btp` + xcframework + app, sign with Developer ID Application "HAPPYSOLVERS SAS", notarize with `notarytool`, staple, upload `btp-<v>-macos-universal.tar.gz` and `BalanceTonPort-<v>.zip` to the GitHub release), `packaging/homebrew/btp.rb`, `packaging/homebrew/balance-ton-port.rb` (cask, `depends_on macos: ">= :sonoma"`)
- Secrets required (user action, documented in README "Releasing"): `APPLE_CERT_P12_BASE64`, `APPLE_CERT_PASSWORD`, `APPLE_TEAM_ID`, `APPLE_ID`, `APPLE_APP_PASSWORD`.

- [ ] **Step 0:** Ask the user before creating the public GitHub repo `matthieuh/balance-ton-port` (`gh repo create matthieuh/balance-ton-port --public --source . --push`).
- [ ] **Step 1:** CI workflow green on a pushed branch.
- [ ] **Step 2:** With secrets set by the user, tag `v0.1.0`: release assets uploaded, `spctl -a -vv` on the downloaded app says "accepted, source=Notarized Developer ID".
- [ ] **Step 3:** Publish formula + cask to `matthieuh/homebrew-tap` (user creates the repo); `brew install matthieuh/tap/btp` and `brew install --cask matthieuh/tap/balance-ton-port` work on a clean shell.
- [ ] **Step 4:** Commit `ci: release pipeline and Homebrew packaging`.
