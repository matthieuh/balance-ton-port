//! Codex CLI: recognising its processes from the command line.

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
}
