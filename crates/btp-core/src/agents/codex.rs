//! Codex CLI: recognising its processes from the command line, and finding
//! the live sessions among them. A native `codex` process holds
//! `<CODEX_HOME>/thread-writer-locks/<thread id>.lock` open for every thread
//! it writes; that is what ties a process to its conversations.

use super::codex_rollout::{newest_rollout_for_cwd, read_meta, rollout_for_thread, SessionMeta};
use super::{is_agent_cli, Agent, AgentKind, AgentSession};
use crate::platform::{ancestry, format_proc_start, Platform};
use crate::sessions::SessionStatus;
use std::path::{Path, PathBuf};

/// What a Codex-looking process is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexRole {
    /// The TUI (no subcommand, `resume`, `fork`, or a bare prompt).
    Interactive,
    /// `exec`, `review`: a live session without a TUI.
    NonInteractive,
    /// A launcher that stays the parent of the native binary (npm `codex.js`,
    /// a shell script named `codex`).
    Wrapper,
    /// A helper or desktop-app process that is not a session of its own.
    Service,
}

// Lists taken from the upstream CLI definitions (openai/codex):
// `codex-rs/cli/src/main.rs` (subcommands and global flags) and
// `codex-rs/tui/src/cli.rs` (TUI flags). `codex-cli/bin/codex.js` is the npm
// wrapper.

/// Flags that take the next word as their value unless written `--flag=value`.
const VALUE_FLAGS: &[&str] = &[
    "-c",
    "--config",
    "-m",
    "--model",
    "-p",
    "--profile",
    "-s",
    "--sandbox",
    "-a",
    "--ask-for-approval",
    "-C",
    "--cd",
    "-i",
    "--image",
    "--add-dir",
    "--enable",
    "--disable",
    "--local-provider",
];

const NON_INTERACTIVE: &[&str] = &["exec", "e", "review"];

const SERVICES: &[&str] = &[
    "app-server",
    "exec-server",
    "mcp",
    "mcp-server",
    "login",
    "logout",
    "sandbox",
    "debug",
    "completion",
    "cloud",
    "apply",
    "a",
    "doctor",
    "features",
    "proto",
];

fn base(w: &str) -> &str {
    w.rsplit('/').next().unwrap_or("")
}

/// Classify `command` (space-joined argv) as a Codex process, if it is one.
/// An unknown word after the flags is a prompt, hence `Interactive`.
pub fn classify_codex(command: &str) -> Option<CodexRole> {
    let mut words = command.split_whitespace();
    let argv0 = base(words.next()?);
    match argv0 {
        "codex" => Some(native_role(words)),
        "node" | "nodejs" | "bun" => words
            .find(|w| !w.starts_with('-'))
            .filter(|w| w.ends_with("@openai/codex/bin/codex.js"))
            .map(|_| CodexRole::Wrapper),
        "bash" | "sh" | "zsh" => words
            .find(|w| !w.starts_with('-'))
            .filter(|w| base(w) == "codex")
            .map(|_| CodexRole::Wrapper),
        _ => None,
    }
}

fn native_role<'a>(mut words: impl Iterator<Item = &'a str>) -> CodexRole {
    while let Some(w) = words.next() {
        if w.starts_with('-') {
            if VALUE_FLAGS.contains(&w) {
                words.next();
            }
            continue;
        }
        return if NON_INTERACTIVE.contains(&w) {
            CodexRole::NonInteractive
        } else if SERVICES.contains(&w) {
            CodexRole::Service
        } else {
            CodexRole::Interactive
        };
    }
    CodexRole::Interactive
}

pub struct CodexAgent {
    /// `~/.codex`, used when the process env names no usable `CODEX_HOME`.
    pub default_home: Option<PathBuf>,
}

fn canonical(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// `CODEX_HOME` from the process env when it names an existing directory
/// (canonicalized), else `default_home` when that exists.
pub fn codex_home(
    env: Option<&[(String, String)]>,
    default_home: Option<&Path>,
) -> Option<PathBuf> {
    let from_env = env
        .and_then(|e| e.iter().find(|(k, _)| k == "CODEX_HOME"))
        .map(|(_, v)| PathBuf::from(v))
        .filter(|p| p.is_dir());
    from_env
        .or_else(|| default_home.filter(|p| p.is_dir()).map(Path::to_path_buf))
        .map(|p| canonical(&p))
}

/// Thread ids of the open files `<codex_home>/thread-writer-locks/<id>.lock`,
/// in path order. Paths are compared canonicalized (`/tmp` vs `/private/tmp`).
pub fn held_threads(open: &[PathBuf], codex_home: &Path) -> Vec<String> {
    let locks = canonical(codex_home).join("thread-writer-locks");
    let mut found: Vec<(PathBuf, String)> = open
        .iter()
        .filter(|p| p.extension().is_some_and(|x| x == "lock"))
        .filter_map(|p| {
            let c = canonical(p);
            if c.parent() != Some(locks.as_path()) {
                return None;
            }
            let id = c.file_stem()?.to_str()?.to_string();
            Some((c, id))
        })
        .collect();
    found.sort();
    found.dedup();
    found.into_iter().map(|(_, id)| id).collect()
}

impl CodexAgent {
    /// Live Codex sessions. A `codex exec` started below a process that is
    /// itself a live session in `outer` belongs to that session and is left out.
    pub fn live_sessions_nested(
        &self,
        p: &dyn Platform,
        outer: &[AgentSession],
    ) -> Vec<AgentSession> {
        let found: Vec<(CodexRole, AgentSession)> = p
            .pids_named("codex")
            .into_iter()
            .filter_map(|pid| self.session_of(p, pid, outer))
            .collect();
        // Interactive sessions are never nested; an `exec` below one belongs to it.
        let tuis: Vec<AgentSession> = found
            .iter()
            .filter(|(r, _)| *r == CodexRole::Interactive)
            .map(|(_, s)| s.clone())
            .collect();
        found
            .into_iter()
            .filter(|(r, s)| *r == CodexRole::Interactive || !nested_in_session(p, s.pid, &tuis))
            .map(|(_, s)| s)
            .collect()
    }

    fn session_of(
        &self,
        p: &dyn Platform,
        pid: i32,
        outer: &[AgentSession],
    ) -> Option<(CodexRole, AgentSession)> {
        let info = p.proc_info(pid)?;
        let role = classify_codex(&info.command)?;
        if !matches!(role, CodexRole::Interactive | CodexRole::NonInteractive) {
            return None;
        }
        if role == CodexRole::NonInteractive && nested_in_session(p, pid, outer) {
            return None;
        }
        let home = codex_home(p.proc_env(pid).as_deref(), self.default_home.as_deref());
        let held = home
            .as_deref()
            .map(|h| held_threads(&p.open_paths(pid), h))
            .unwrap_or_default();
        let start_ms = info.start_time / 1000;
        let mut conversation_id = String::new();
        let mut others = Vec::new();
        let mut meta: Option<SessionMeta> = None;
        if let (Some(home), false) = (home.as_deref(), held.is_empty()) {
            let metas: Vec<Option<SessionMeta>> = held
                .iter()
                .map(|t| rollout_for_thread(home, t).and_then(|r| read_meta(&r)))
                .collect();
            let main = metas
                .iter()
                .position(|m| m.as_ref().is_some_and(|m| m.parent_thread_id.is_none()))
                .unwrap_or(0);
            conversation_id = held[main].clone();
            others = held
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != main)
                .map(|(_, t)| t.clone())
                .collect();
            meta = metas.into_iter().nth(main).flatten();
        } else if let (Some(home), Some(cwd)) = (home.as_deref(), info.cwd.as_deref()) {
            if let Some((_, m)) = newest_rollout_for_cwd(home, cwd, start_ms) {
                conversation_id = m.id.clone();
                meta = Some(m);
            }
        }
        let cwd = meta.map(|m| m.cwd).or(info.cwd).unwrap_or_default();
        let name = cwd
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let session = AgentSession {
            agent: AgentKind::Codex,
            pid,
            proc_start: format_proc_start(info.start_time),
            conversation_id,
            other_conversations: others,
            cwd,
            name,
            status: SessionStatus::Unknown,
            status_updated_at_ms: start_ms,
        };
        Some((role, session))
    }
}

/// An ancestor of `pid` is an agent CLI that is a live session in `outer`.
fn nested_in_session(p: &dyn Platform, pid: i32, outer: &[AgentSession]) -> bool {
    ancestry(p, pid).iter().skip(1).any(|a| {
        is_agent_cli(&a.command).is_some()
            && outer
                .iter()
                .any(|s| s.pid == a.pid && s.proc_start == format_proc_start(a.start_time))
    })
}

impl Agent for CodexAgent {
    fn kind(&self) -> AgentKind {
        AgentKind::Codex
    }

    fn is_cli(&self, command: &str) -> bool {
        classify_codex(command).is_some()
    }

    fn live_sessions(&self, p: &dyn Platform) -> Vec<AgentSession> {
        self.live_sessions_nested(p, &[])
    }

    fn live_sessions_after(&self, p: &dyn Platform, outer: &[AgentSession]) -> Vec<AgentSession> {
        self.live_sessions_nested(p, outer)
    }

    fn env_conversation(&self, env: &[(String, String)]) -> Option<String> {
        env_thread(env)
    }
}

/// `CODEX_THREAD_ID` from a process environment: the only value read from it.
pub fn env_thread(env: &[(String, String)]) -> Option<String> {
    env.iter()
        .find(|(k, _)| k == "CODEX_THREAD_ID")
        .map(|(_, v)| v.clone())
        .filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use CodexRole::*;

    fn all(cmds: &[&str], want: CodexRole) {
        for c in cmds {
            assert_eq!(classify_codex(c), Some(want), "{c}");
        }
    }

    #[test]
    fn native_interactive_forms() {
        all(
            &[
                "/Users/x/.local/bin/codex",
                "codex \"fix the build\"",
                "codex -m gpt-5 resume --last",
                "codex fork abc",
                "codex --cd /tmp/w",
                // Real argv: the -c value (braces, quotes) is one word, --enable takes a value.
                "/Users/x/.local/bin/codex -c projects={\"/w\"={trust_level=\"trusted\"}} --enable hooks --dangerously-bypass-hook-trust",
            ],
            Interactive,
        );
    }

    #[test]
    fn exec_and_review_are_non_interactive() {
        all(
            &[
                "codex exec \"run tests\"",
                "codex -c model=x exec --json hi",
                "codex review",
            ],
            NonInteractive,
        );
    }

    #[test]
    fn services_are_not_sessions() {
        all(
            &[
                "codex app-server",
                "codex exec-server",
                "codex mcp-server",
                "codex login",
                "/Applications/ChatGPT.app/Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex app-server --listen stdio://",
                "/Applications/ChatGPT.app/Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex sandbox -c x=y",
                "/Applications/ChatGPT.app/Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex -c forced_login_method=\"chatgpt\" exec-server --remote https://example",
            ],
            Service,
        );
    }

    #[test]
    fn flag_values_are_not_subcommands() {
        assert_eq!(classify_codex("codex -m exec"), Some(Interactive));
        assert_eq!(
            classify_codex("codex --model=exec exec x"),
            Some(NonInteractive)
        );
    }

    #[test]
    fn npm_and_shell_wrappers() {
        all(
            &[
                "node /opt/homebrew/lib/node_modules/@openai/codex/bin/codex.js resume",
                "/bin/bash /Users/x/.superset/bin/codex",
            ],
            Wrapper,
        );
    }

    #[test]
    fn not_codex() {
        for c in [
            "node codex-dashboard.js",
            "/usr/local/bin/codexx",
            "vim codex.md",
            "python -m codex",
            "codex-code-mode-host",
            "/opt/x/codex-code-mode-host --flag",
            "",
        ] {
            assert_eq!(classify_codex(c), None, "{c}");
        }
    }

    mod agent {
        use super::super::*;
        use crate::agents::codex_rollout::civil_from_days;
        use crate::config::Config;
        use crate::platform::FakePlatform;
        use std::fs;

        const T1: &str = "11111111-1111-4111-8111-111111111111";
        const T2: &str = "22222222-2222-4222-8222-222222222222";
        const START_US: u64 = 1_792_660_000_000_000;

        fn meta_line(id: &str, cwd: &str, parent: Option<&str>) -> String {
            let parent = parent
                .map(|p| format!(r#","parent_thread_id":"{p}""#))
                .unwrap_or_default();
            format!(
                r#"{{"type":"session_meta","payload":{{"id":"{id}","timestamp":"2026-10-22T09:15:02.123Z","cwd":"{cwd}"{parent}}}}}"#
            )
        }

        fn rollout(home: &Path, id: &str, cwd: &str, parent: Option<&str>) {
            let day = home.join("sessions/2026/10/22");
            fs::create_dir_all(&day).unwrap();
            fs::write(
                day.join(format!("rollout-2026-10-22T11-15-02-{id}.jsonl")),
                format!("{}\n", meta_line(id, cwd, parent)),
            )
            .unwrap();
        }

        fn lock(home: &Path, id: &str) -> String {
            let dir = home.join("thread-writer-locks");
            fs::create_dir_all(&dir).unwrap();
            let f = dir.join(format!("{id}.lock"));
            fs::write(&f, "").unwrap();
            f.to_string_lossy().into_owned()
        }

        fn agent(home: &Path) -> CodexAgent {
            CodexAgent {
                default_home: Some(home.to_path_buf()),
            }
        }

        #[test]
        fn native_codex_with_lock_is_a_live_session() {
            let home = tempfile::tempdir().unwrap();
            rollout(home.path(), T1, "/Users/test/web", None);
            let l = lock(home.path(), T1);
            let p = FakePlatform::new();
            p.spawn(
                300,
                1,
                START_US,
                "/Users/test/.local/bin/codex",
                "/Users/test/web",
            );
            p.set_open_paths(300, &[&l]);
            let s = agent(home.path()).live_sessions(&p);
            assert_eq!(s.len(), 1);
            assert_eq!(s[0].agent, AgentKind::Codex);
            assert_eq!(s[0].conversation_id, T1);
            assert_eq!(s[0].proc_start, format_proc_start(START_US));
            assert_eq!(s[0].name, "web");
            assert_eq!(s[0].status, SessionStatus::Unknown);
            assert_eq!(s[0].status_updated_at_ms, START_US / 1000);
        }

        #[test]
        fn codex_home_from_env_else_default() {
            let tmp = tempfile::tempdir().unwrap();
            let account = tmp.path().join("account");
            let default = tmp.path().join("default");
            fs::create_dir_all(&account).unwrap();
            fs::create_dir_all(&default).unwrap();
            let env = vec![(
                "CODEX_HOME".to_string(),
                account.to_string_lossy().into_owned(),
            )];
            assert_eq!(
                codex_home(Some(&env), Some(&default)),
                Some(account.canonicalize().unwrap())
            );
            let missing = vec![(
                "CODEX_HOME".to_string(),
                tmp.path().join("nope").to_string_lossy().into_owned(),
            )];
            assert_eq!(
                codex_home(Some(&missing), Some(&default)),
                Some(default.canonicalize().unwrap())
            );
            assert_eq!(codex_home(None, None), None);
        }

        #[test]
        fn fallback_to_newest_rollout_by_cwd() {
            let home = tempfile::tempdir().unwrap();
            let cwd = tempfile::tempdir().unwrap();
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64;
            let (y, m, d) = civil_from_days((now_ms / 86_400_000) as i64);
            let day = home.path().join(format!("sessions/{y:04}/{m:02}/{d:02}"));
            fs::create_dir_all(&day).unwrap();
            let cwd_s = cwd.path().to_string_lossy().into_owned();
            fs::write(
                day.join(format!("rollout-x-{T2}.jsonl")),
                format!("{}\n", meta_line(T2, &cwd_s, None)),
            )
            .unwrap();
            let p = FakePlatform::new();
            p.spawn(300, 1, (now_ms - 60_000) * 1000, "codex", &cwd_s);
            let s = agent(home.path()).live_sessions(&p);
            assert_eq!(s.len(), 1);
            assert_eq!(s[0].conversation_id, T2);
        }

        #[test]
        fn session_without_rollout_is_unknown_and_still_links() {
            let home = tempfile::tempdir().unwrap();
            let p = FakePlatform::new();
            p.spawn(300, 1, START_US, "codex", "/Users/test/web");
            let s = agent(home.path()).live_sessions(&p);
            assert_eq!(s.len(), 1);
            assert_eq!(s[0].conversation_id, "");
            assert_eq!(s[0].status, SessionStatus::Unknown);
            assert_eq!(s[0].cwd, PathBuf::from("/Users/test/web"));

            // A server below the session links to it as a Codex server.
            p.spawn(
                301,
                300,
                START_US + 1_000_000,
                "bun run dev",
                "/Users/test/web",
            );
            p.add_listener(301, 3000);
            let listener = crate::platform::Listener {
                pid: 301,
                port: 3000,
            };
            let chain = ancestry(&p, 301);
            let server =
                crate::watcher::link(&listener, &chain, &|_| None, &s, &[], &Config::default(), 0)
                    .expect("linked");
            assert_eq!(server.agent, AgentKind::Codex);
        }

        #[test]
        fn stale_lock_file_without_holder_is_ignored() {
            let home = tempfile::tempdir().unwrap();
            lock(home.path(), T1);
            let p = FakePlatform::new();
            assert!(agent(home.path()).live_sessions(&p).is_empty());
        }

        #[test]
        fn wrapper_and_services_are_not_sessions() {
            let home = tempfile::tempdir().unwrap();
            rollout(home.path(), T1, "/Users/test/web", None);
            let l = lock(home.path(), T1);
            let p = FakePlatform::new();
            p.spawn(
                100,
                1,
                START_US,
                "node /opt/lib/node_modules/@openai/codex/bin/codex.js",
                "/Users/test/web",
            );
            p.spawn(
                300,
                100,
                START_US,
                "/Users/test/.local/bin/codex",
                "/Users/test/web",
            );
            p.set_open_paths(300, &[&l]);
            p.spawn(
                400,
                1,
                START_US,
                "/Applications/ChatGPT.app/CodexCLI.app/Contents/MacOS/codex app-server",
                "/",
            );
            let s = agent(home.path()).live_sessions(&p);
            assert_eq!(s.len(), 1);
            assert_eq!(s[0].pid, 300);
        }

        #[test]
        fn nested_exec_inside_claude_tree_is_not_a_session() {
            let home = tempfile::tempdir().unwrap();
            let p = FakePlatform::new();
            p.spawn(10, 1, START_US, "claude", "/Users/test/web");
            p.spawn(11, 10, START_US, "zsh", "/Users/test/web");
            p.spawn(12, 11, START_US, "codex exec fix", "/Users/test/web");
            let outer = AgentSession {
                agent: AgentKind::ClaudeCode,
                pid: 10,
                proc_start: format_proc_start(START_US),
                conversation_id: "c".into(),
                other_conversations: Vec::new(),
                cwd: PathBuf::from("/Users/test/web"),
                name: String::new(),
                status: SessionStatus::Busy,
                status_updated_at_ms: 0,
            };
            let a = agent(home.path());
            assert!(a.live_sessions_nested(&p, &[outer]).is_empty());
            // Without a live outer session it is a session of its own.
            assert_eq!(a.live_sessions(&p).len(), 1);
        }

        #[test]
        fn nested_exec_inside_codex_tui_is_not_a_session() {
            let home = tempfile::tempdir().unwrap();
            let p = FakePlatform::new();
            p.spawn(
                300,
                1,
                START_US,
                "/Users/test/.local/bin/codex",
                "/Users/test/web",
            );
            p.spawn(301, 300, START_US, "zsh", "/Users/test/web");
            p.spawn(302, 301, START_US, "codex exec fix", "/Users/test/web");
            let s = agent(home.path()).live_sessions(&p);
            assert_eq!(s.len(), 1);
            assert_eq!(s[0].pid, 300);
        }

        #[test]
        fn sub_agent_threads_are_other_conversations() {
            let home = tempfile::tempdir().unwrap();
            rollout(home.path(), T1, "/Users/test/web", None);
            rollout(home.path(), T2, "/Users/test/web", Some(T1));
            let (l2, l1) = (lock(home.path(), T2), lock(home.path(), T1));
            let p = FakePlatform::new();
            p.spawn(300, 1, START_US, "codex", "/Users/test/web");
            p.set_open_paths(300, &[&l2, &l1]);
            let s = agent(home.path()).live_sessions(&p);
            assert_eq!(s[0].conversation_id, T1);
            assert_eq!(s[0].other_conversations, vec![T2.to_string()]);
        }

        #[test]
        fn env_conversation_is_the_thread_id() {
            let a = CodexAgent { default_home: None };
            let env = vec![("CODEX_THREAD_ID".to_string(), T1.to_string())];
            assert_eq!(a.env_conversation(&env).as_deref(), Some(T1));
            assert_eq!(a.env_conversation(&[]), None);
        }
    }
}
