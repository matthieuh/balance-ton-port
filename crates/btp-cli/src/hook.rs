//! `btp hook pre-bash`: the Claude Code PreToolUse hook on the Bash tool.
//! It acts only when the command starts a known dev server, applies the
//! `port_conflict` rule to the port, and either lets the command run
//! (silently) or denies it with a message for the agent. It never rewrites
//! the command and never fails: any error or panic is a silent exit 0.

use crate::run::holder_text;
use crate::{new_watcher, now_ms};
use btp_core::config::{data_dir, read_config};
use btp_core::run::command::{recognize, PortStyle};
use btp_core::run::conflict::{resolve, Resolution, RunFlags};
use std::io::Read;
use std::path::{Path, PathBuf};

/// Reads the PreToolUse payload on stdin; prints the deny decision when the
/// command must not run. Always returns the exit code 0.
pub fn pre_bash() -> u8 {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return 0;
    }
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let decided = std::panic::catch_unwind(|| decide(&input));
    std::panic::set_hook(hook);
    if let Ok(Some(reason)) = decided {
        let out = serde_json::json!({"hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }});
        println!("{out}");
    }
    0
}

/// The reason to deny with, or `None` to let the command run.
fn decide(input: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(input).ok()?;
    if v.get("tool_name")?.as_str()? != "Bash" {
        return None;
    }
    let command = v.get("tool_input")?.get("command")?.as_str()?;
    let cwd = v
        .get("cwd")
        .and_then(|c| c.as_str())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())?;
    let (found, dev) = find_commands(command, &cwd)?.into_iter().find_map(|f| {
        let env_port = f.env_port.clone().or_else(|| std::env::var("PORT").ok());
        let dev = recognize(&f.argv, &f.cwd, env_port.as_deref())?;
        Some((f, dev))
    })?;

    let dir = data_dir();
    // A broken config never blocks the agent.
    let cfg = read_config(&dir).ok()?;
    if !cfg.port_check_hook {
        return None;
    }
    let mut w = new_watcher(dir);
    let now = now_ms();
    // A port btp cannot read: nothing to check.
    let port = dev.port?;
    let holder = w.port_holder(port, now).ok()??;
    let res = resolve(
        &holder,
        dev.style.is_some(),
        cfg.port_conflict,
        RunFlags::default(),
        &cfg,
        now,
    );
    if let Resolution::Free(key) = res {
        if matches!(w.free_for_run(key, now), Ok(Some(_))) {
            return None;
        }
    }
    // The shell wrapper reroutes the command itself.
    if std::env::var("BTP_SHELL_WRAP").as_deref() == Ok("1") {
        return None;
    }
    let who = holder_text(&holder);
    let next = port.saturating_add(1);
    let cmd = shell_join(&found.argv);
    let mut advice = match dev.style {
        Some(PortStyle::LongFlag) => {
            format!("Rerun as `btp run -- {cmd}` to get a free port, or pass --port {next}.")
        }
        Some(PortStyle::ShortFlag) => {
            format!("Rerun as `btp run -- {cmd}` to get a free port, or pass -p {next}.")
        }
        Some(PortStyle::Env) => {
            format!("Rerun as `btp run -- {cmd}` to get a free port, or set PORT={next}.")
        }
        // `btp run` could not reroute it either.
        None => "Start it on a free port (btp cannot change this command's port), or ask the user."
            .to_string(),
    };
    if matches!(res, Resolution::Ask { can_kill: true }) {
        advice.push_str(" Ask the user before freeing it.");
    }
    Some(format!("port {port} is used by {who}. {advice}"))
}

/// The words quoted so a shell reads them back as they are.
fn shell_join(argv: &[String]) -> String {
    let safe = |w: &str| {
        !w.is_empty()
            && w.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_./=:@%+,".contains(c))
    };
    argv.iter()
        .map(|w| {
            if safe(w) {
                w.clone()
            } else {
                format!("'{}'", w.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// One simple command of a shell line, as the hook reads it.
#[derive(Debug, PartialEq, Eq)]
pub struct Found {
    /// The command words, without `cd`, assignments and redirections.
    pub argv: Vec<String>,
    /// The folder it runs in: the hook's cwd moved by the `cd`s before it.
    pub cwd: PathBuf,
    /// The `PORT=` assigned on this command, if any.
    pub env_port: Option<String>,
}

#[derive(Debug)]
struct Word {
    text: String,
    quoted: bool,
}

#[derive(Debug, Default)]
struct Segment {
    words: Vec<Word>,
    /// Holds an expansion (`$`) btp cannot resolve.
    dirty: bool,
}

/// Splits a shell line on `&&`, `||`, `;`, `|`, `&` and newlines, honoring
/// quotes. `None` for anything it cannot read safely (unterminated quote,
/// subshell, backticks).
fn split(line: &str) -> Option<Vec<Segment>> {
    let mut segs = vec![Segment::default()];
    let mut cur = String::new();
    let mut in_word = false;
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    fn end_word(segs: &mut [Segment], cur: &mut String, in_word: &mut bool, quoted: &mut bool) {
        if *in_word {
            segs.last_mut().unwrap().words.push(Word {
                text: std::mem::take(cur),
                quoted: *quoted,
            });
        }
        *in_word = false;
        *quoted = false;
    }
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                quoted = true;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        ch => cur.push(ch),
                    }
                }
            }
            '"' => {
                in_word = true;
                quoted = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => match chars.next()? {
                            e @ ('"' | '\\' | '$' | '`') => cur.push(e),
                            e => {
                                cur.push('\\');
                                cur.push(e);
                            }
                        },
                        '`' => return None,
                        '$' => {
                            segs.last_mut().unwrap().dirty = true;
                            cur.push('$');
                        }
                        ch => cur.push(ch),
                    }
                }
            }
            '\\' => {
                in_word = true;
                let e = chars.next()?;
                if e != '\n' {
                    cur.push(e);
                }
            }
            '$' => {
                in_word = true;
                segs.last_mut().unwrap().dirty = true;
                cur.push('$');
            }
            '`' | '(' | ')' => return None,
            // A heredoc or here-string: its body is data, not commands.
            '<' if chars.peek() == Some(&'<') => return None,
            // A comment runs to the end of the line.
            '#' if !in_word => {
                while chars.peek().is_some_and(|&n| n != '\n') {
                    chars.next();
                }
            }
            ' ' | '\t' => end_word(&mut segs, &mut cur, &mut in_word, &mut quoted),
            ';' | '\n' | '&' | '|' => {
                end_word(&mut segs, &mut cur, &mut in_word, &mut quoted);
                if matches!(c, '&' | '|') && chars.peek() == Some(&c) {
                    chars.next();
                }
                segs.push(Segment::default());
            }
            ch => {
                in_word = true;
                cur.push(ch);
            }
        }
    }
    end_word(&mut segs, &mut cur, &mut in_word, &mut quoted);
    Some(segs)
}

/// `NAME=value` (an assignment word).
fn assignment(w: &str) -> Option<(&str, &str)> {
    let (n, v) = w.split_once('=')?;
    let mut cs = n.chars();
    let first = cs.next()?;
    (first.is_ascii_alphabetic() || first == '_')
        .then_some(())
        .filter(|_| cs.all(|c| c.is_ascii_alphanumeric() || c == '_'))?;
    Some((n, v))
}

/// An unquoted redirection operator word (`>`, `2>`, `>>`, `<`, `>&2`...),
/// and whether its target is the next word.
fn redirection(w: &Word) -> Option<bool> {
    if w.quoted {
        return None;
    }
    let t = w.text.trim_start_matches(|c: char| c.is_ascii_digit());
    let rest = t
        .strip_prefix(">>")
        .or_else(|| t.strip_prefix('>'))
        .or_else(|| t.strip_prefix('<'))?;
    Some(rest.is_empty())
}

/// The simple commands of `line` that are not a `cd`, in order, each with
/// the folder it runs in. `None` when the line cannot be read safely.
pub fn find_commands(line: &str, cwd: &Path) -> Option<Vec<Found>> {
    let mut cwd = cwd.to_path_buf();
    let mut out = Vec::new();
    for seg in split(line)? {
        let mut words = seg.words.iter().peekable();
        let mut env_port = None;
        while let Some((n, v)) = words.peek().and_then(|w| assignment(&w.text)) {
            if n == "PORT" {
                env_port = Some(v.to_string());
            }
            words.next();
        }
        let mut argv = Vec::new();
        while let Some(w) = words.next() {
            match redirection(w) {
                Some(true) => {
                    words.next();
                }
                Some(false) => {}
                None => argv.push(w.text.clone()),
            }
        }
        if argv.is_empty() {
            continue;
        }
        if argv[0] == "cd" {
            if seg.dirty || argv.len() != 2 || argv[1].starts_with(['~', '-']) {
                return None;
            }
            let to = cwd.join(&argv[1]);
            if !to.is_dir() {
                return None;
            }
            cwd = to;
            continue;
        }
        if seg.dirty {
            continue;
        }
        out.push(Found {
            argv,
            cwd: cwd.clone(),
            env_port,
        });
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(line: &str) -> Option<Found> {
        find_commands(line, Path::new("/tmp"))?.into_iter().next()
    }

    fn words(f: &Found) -> Vec<&str> {
        f.argv.iter().map(String::as_str).collect()
    }

    #[test]
    fn plain_command() {
        let f = find("npm run dev").unwrap();
        assert_eq!(words(&f), ["npm", "run", "dev"]);
        assert_eq!(f.cwd, Path::new("/tmp"));
    }

    #[test]
    fn cd_moves_cwd_and_assignments_are_stripped() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("web")).unwrap();
        let f = find_commands("cd web && PORT=3000 npm run dev", d.path())
            .unwrap()
            .remove(0);
        assert_eq!(f.cwd, d.path().join("web"));
        assert_eq!(f.env_port.as_deref(), Some("3000"));
        assert_eq!(words(&f), ["npm", "run", "dev"]);
    }

    #[test]
    fn cd_to_a_missing_folder_is_unreadable() {
        assert_eq!(find("cd /nonexistent-btp && npm run dev"), None);
    }

    #[test]
    fn quotes_and_separators() {
        let f = find("echo 'a && b' ; vite --host \"my host\" | tee log").unwrap();
        assert_eq!(words(&f), ["echo", "a && b"]);
        let all = find_commands("false || vite --port 4000 &", Path::new("/tmp")).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(words(&all[1]), ["vite", "--port", "4000"]);
    }

    #[test]
    fn redirections_are_dropped() {
        let f = find("npm run dev > out.log 2>&1 &").unwrap();
        assert_eq!(words(&f), ["npm", "run", "dev"]);
    }

    #[test]
    fn unreadable_lines() {
        assert_eq!(find("echo 'unterminated"), None);
        assert_eq!(find("(cd web && npm run dev)"), None);
        assert_eq!(find("echo `date`"), None);
        assert_eq!(find("cd $DIR && npm run dev"), None);
    }

    #[test]
    fn expansions_skip_only_their_command() {
        let f = find("echo $HOME && vite").unwrap();
        assert_eq!(words(&f), ["vite"]);
    }

    #[test]
    fn heredocs_are_unreadable() {
        assert_eq!(find("cat > f <<'EOF'\nvite --port 1\nEOF"), None);
        assert_eq!(find("cat <<-EOF\nvite\nEOF"), None);
        assert_eq!(find("cat <<< vite"), None);
    }

    #[test]
    fn comments_are_skipped() {
        let all = find_commands("ls # x; vite\nnpm run dev # y", Path::new("/tmp")).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(words(&all[0]), ["ls"]);
        assert_eq!(words(&all[1]), ["npm", "run", "dev"]);
        // Not a comment inside a word or quotes.
        let f = find("echo a#b '# c'").unwrap();
        assert_eq!(words(&f), ["echo", "a#b", "# c"]);
    }

    #[test]
    fn shell_join_quotes() {
        let a = |w: &[&str]| shell_join(&w.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(a(&["npm", "run", "dev"]), "npm run dev");
        assert_eq!(a(&["echo", "a b", "it's"]), "echo 'a b' 'it'\\''s'");
    }
}
