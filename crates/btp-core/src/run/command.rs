//! Recognizing a dev-server command line and rewriting its port.
//!
//! Pure apart from reading `cwd/package.json` to resolve package-manager
//! scripts (`npm run dev`). The framework comes from the program word only
//! (see `program_at`), never from an argument, so `ls node_modules/.bin/vite`
//! or `rm -rf .next` is not a dev server. This module also knows the default
//! port, how the port is set, and the rewrite.

use std::path::Path;

/// How a framework takes its port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortStyle {
    /// `--port N`
    LongFlag,
    /// `-p N`
    ShortFlag,
    /// `PORT=N` in the environment
    Env,
}

/// What starts the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Launcher {
    Direct,
    Npm,
    Pnpm,
    Yarn,
    Bun,
    Npx,
}

/// Where the intended port came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortSource {
    /// A flag on the command line.
    Argv,
    /// A flag or `PORT=` inside the package.json script.
    Script,
    /// The `PORT` environment variable.
    Env,
    /// The framework's default.
    Default,
}

/// A recognized dev-server command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevCommand {
    pub framework: String,
    /// `None`: the command sets its port with a value btp cannot read (a
    /// port flag whose value is not a number, such as `--port $PORT` in a
    /// script). The port is unknown: no check, no rewrite.
    pub port: Option<u16>,
    pub port_from: PortSource,
    /// How to set the port. `None`: unknown, the command cannot be rerouted.
    pub style: Option<PortStyle>,
    pub launcher: Launcher,
    /// The package.json script, for a package-manager launcher.
    pub script: Option<String>,
}

/// One known framework: its name, the programs that start it, its default
/// port and the way it takes a port. Add a row to recognize one more.
struct Entry {
    framework: &'static str,
    /// Program names (basename, a JavaScript extension removed).
    programs: &'static [&'static str],
    default_port: u16,
    style: PortStyle,
    /// Subcommands that serve; `""` stands for none given. Empty: the
    /// program always serves (`remix-serve <build>`).
    serving: &'static [&'static str],
    /// A flag that makes the command serve, for tools with no subcommand.
    serving_flag: Option<&'static str>,
    /// The framework takes its port from `PORT` when no flag sets it. Only
    /// then does an inherited or inline `PORT` count as the intended port.
    reads_port_env: bool,
}

const fn entry(
    framework: &'static str,
    programs: &'static [&'static str],
    default_port: u16,
    style: PortStyle,
    serving: &'static [&'static str],
) -> Entry {
    Entry {
        framework,
        programs,
        default_port,
        style,
        serving,
        serving_flag: None,
        reads_port_env: false,
    }
}

/// `reads_port_env`, checked in each tool's CLI source: Next.js declares
/// `-p, --port` with `.env('PORT')` (commander) for `dev` and `start`;
/// Nuxt's `nuxi dev` reads `NUXT_PORT`, `NITRO_PORT`, then `PORT`;
/// `remix-serve` listens on `PORT` (it has no port flag). Vite (so
/// SvelteKit and `remix vite:dev`), Astro, wrangler, Expo (it reads
/// `RCT_METRO_PORT`), Storybook and Eleventy ignore `PORT`.
const FRAMEWORKS: &[Entry] = &[
    entry(
        "Vite",
        &["vite"],
        5173,
        PortStyle::LongFlag,
        &["", "dev", "serve", "preview"],
    ),
    Entry {
        reads_port_env: true,
        ..entry(
            "Next.js",
            &["next"],
            3000,
            PortStyle::ShortFlag,
            &["", "dev", "start"],
        )
    },
    entry(
        "Astro",
        &["astro"],
        4321,
        PortStyle::LongFlag,
        &["dev", "preview"],
    ),
    entry("Expo", &["expo"], 8081, PortStyle::LongFlag, &["start"]),
    entry(
        "wrangler",
        &["wrangler"],
        8787,
        PortStyle::LongFlag,
        &["dev"],
    ),
    entry(
        "SvelteKit",
        &["svelte-kit"],
        5173,
        PortStyle::LongFlag,
        &["", "dev", "preview"],
    ),
    entry(
        "Remix",
        &["remix"],
        3000,
        PortStyle::LongFlag,
        &["dev", "vite:dev"],
    ),
    Entry {
        reads_port_env: true,
        ..entry("Remix", &["remix-serve"], 3000, PortStyle::Env, &[])
    },
    Entry {
        reads_port_env: true,
        ..entry(
            "Nuxt",
            &["nuxt", "nuxi"],
            3000,
            PortStyle::LongFlag,
            &["dev", "preview"],
        )
    },
    Entry {
        serving_flag: Some("--serve"),
        ..entry(
            "Eleventy",
            &["eleventy", "11ty"],
            8080,
            PortStyle::LongFlag,
            &[""],
        )
    },
    entry(
        "Storybook",
        &["storybook", "start-storybook"],
        6006,
        PortStyle::LongFlag,
        &["", "dev"],
    ),
];

/// Whether the command serves: `rest` being the words after the program,
/// its subcommand (the first of them) is one the framework serves with.
fn serves(entry: &Entry, rest: &[String]) -> bool {
    if entry.serving.is_empty() {
        return true;
    }
    let rest = strip_ports(rest);
    if let Some(flag) = entry.serving_flag {
        return rest.iter().any(|w| w == flag);
    }
    // Only the word right after the program can be a subcommand; a flag
    // there means none was given, and later words are flag values.
    let sub = rest.first().map_or("", |w| w.as_str());
    let sub = if sub.starts_with('-') { "" } else { sub };
    entry.serving.contains(&sub)
}

/// Programs that run the script named by their next word.
const RUNTIMES: &[&str] = &["node", "nodejs", "bun", "deno"];

/// Programs that run the package binary named by their next word.
const PACKAGE_RUNNERS: &[&str] = &["npx", "bunx", "pnpx"];

/// The index of the program word in `tokens`: the first word, or the
/// script a runtime (`node`, `bun`, `deno`) runs, its first non-flag word.
/// `None` when a runtime runs nothing.
fn program_at(tokens: &[String]) -> Option<usize> {
    let first = tokens.first()?;
    if !RUNTIMES.contains(&program(first).as_str()) {
        return Some(0);
    }
    tokens
        .iter()
        .skip(1)
        .position(|w| !w.starts_with('-'))
        .map(|i| i + 1)
}

/// The framework row a program word starts: its basename, a JavaScript
/// extension removed (`node_modules/.bin/vite` and `node_modules/vite/bin/
/// vite.js` are `vite`). `.../wrangler/.../cli.js` is wrangler.
fn entry_of(word: &str) -> Option<&'static Entry> {
    let name = match program(word).as_str() {
        "cli" if word.contains("/wrangler/") => "wrangler".to_string(),
        n => n.to_string(),
    };
    FRAMEWORKS
        .iter()
        .find(|e| e.programs.contains(&name.as_str()))
}

/// Frameworks verified to take the LAST of two repeated port flags. Only for
/// these can a reroute append a second flag after one a package.json script
/// already sets. Next.js parses its CLI with commander (`arg` before 15),
/// where a repeated flag overwrites the earlier value, so the last `-p` wins. The others use
/// parsers that may turn a repeated flag into an array, so they are not
/// assumed.
const LAST_FLAG_WINS: &[&str] = &["Next.js"];

/// A resolved command: the words that start the server, with the package
/// script expanded.
struct Resolved {
    launcher: Launcher,
    script: Option<String>,
    /// Program words (the script text for a package-manager launcher, the
    /// argv without `npx` otherwise), leading `K=V` words removed.
    tokens: Vec<String>,
    /// Words the user added after the script name.
    extra: Vec<String>,
    /// `PORT=N` set inline by the script.
    inline_port: Option<u16>,
}

/// Recognize a dev-server command. `env_port` is the `PORT` the command
/// would inherit.
pub fn recognize(argv: &[String], cwd: &Path, env_port: Option<&str>) -> Option<DevCommand> {
    analyze(argv, cwd, env_port).map(|(d, _)| d)
}

/// A rewritten argv and the environment variables to set.
pub type Rewrite = (Vec<String>, Vec<(String, String)>);

/// The argv for the same command on another port, plus the environment
/// variables to set. `None` when the port cannot be set (`dev.style` is
/// `None`). An existing port flag is replaced in place; otherwise the flag is
/// appended, after `--` for npm, pnpm and bun scripts.
pub fn with_port(argv: &[String], dev: &DevCommand, port: u16) -> Option<Rewrite> {
    let style = dev.style?;
    let p = port.to_string();
    if style == PortStyle::Env {
        return Some((argv.to_vec(), vec![("PORT".to_string(), p)]));
    }
    let mut out = argv.to_vec();
    if replace_ports(&mut out, &p) {
        return Some((out, Vec::new()));
    }
    if matches!(dev.launcher, Launcher::Npm | Launcher::Pnpm | Launcher::Bun)
        && !out.iter().any(|w| w == "--")
    {
        out.push("--".to_string());
    }
    out.push(
        if style == PortStyle::ShortFlag {
            "-p"
        } else {
            "--port"
        }
        .to_string(),
    );
    out.push(p);
    Some((out, Vec::new()))
}

/// The key under which two launches count as the same command: the
/// framework and the resolved command words with any port argument removed.
/// It is folder-free; the caller pairs it with the working folder.
pub fn same_command_key(argv: &[String], cwd: &Path) -> Option<String> {
    let (dev, key_tokens) = analyze(argv, cwd, None)?;
    Some(format!("{}\u{1f}{}", dev.framework, key_tokens.join(" ")))
}

fn analyze(
    argv: &[String],
    cwd: &Path,
    env_port: Option<&str>,
) -> Option<(DevCommand, Vec<String>)> {
    #[cfg(debug_assertions)]
    if let Ok(spec) = std::env::var("BTP_TEST_FRAMEWORK") {
        if let Some(hit) = test_entry(&spec, argv, env_port) {
            return Some(hit);
        }
    }
    let r = resolve(argv, cwd)?;
    let at = program_at(&r.tokens)?;
    let entry = entry_of(&r.tokens[at])?;
    let program_words = &r.tokens[at..];
    if !serves(entry, &program_words[1..]) {
        return None;
    }

    // `Some(None)`: a port flag whose value is not a number.
    let argv_port = last_port(&r.extra).or_else(|| {
        // Direct launches carry the port in the program words themselves.
        if r.script.is_none() {
            last_port(program_words)
        } else {
            None
        }
    });
    let inline_port = r.inline_port.filter(|_| entry.reads_port_env);
    let script_port = if r.script.is_some() {
        last_port(program_words).or(inline_port.map(Some))
    } else {
        None
    };
    let env = env_port
        .filter(|_| entry.reads_port_env)
        .and_then(|p| p.trim().parse::<u16>().ok());
    let (port, port_from) = match (argv_port, script_port, env) {
        (Some(p), _, _) => (p, PortSource::Argv),
        (None, Some(p), _) => (p, PortSource::Script),
        (None, None, Some(p)) => (Some(p), PortSource::Env),
        _ => (Some(entry.default_port), PortSource::Default),
    };

    let mut style = Some(entry.style);
    if port.is_none()
        || port_from == PortSource::Script
            && (inline_port.is_some() || !LAST_FLAG_WINS.contains(&entry.framework))
    {
        style = None;
    }

    let mut key_tokens = strip_ports(program_words);
    key_tokens.extend(strip_ports(&r.extra).into_iter().filter(|w| w != "--"));
    let key_tokens = key_tokens
        .into_iter()
        .map(|w| {
            if !w.starts_with('-') && w.contains('/') {
                base(&w)
            } else {
                w
            }
        })
        .collect();

    Some((
        DevCommand {
            framework: entry.framework.to_string(),
            port,
            port_from,
            style,
            launcher: r.launcher,
            script: r.script,
        },
        key_tokens,
    ))
}

/// Debug builds only, for the CLI tests: `BTP_TEST_FRAMEWORK` set to
/// `<program>:<default port>:<style>` (style `long`, `short`, `env`, or
/// `none` for a command that cannot be rerouted) recognizes any argv with a
/// word whose basename is `<program>`, so a tiny helper server stands for a
/// framework. The port comes from the words after it, then `env_port`, then
/// the default. Never compiled into a release build.
#[cfg(debug_assertions)]
fn test_entry(
    spec: &str,
    argv: &[String],
    env_port: Option<&str>,
) -> Option<(DevCommand, Vec<String>)> {
    let mut parts = spec.rsplitn(3, ':');
    let style = match parts.next()? {
        "long" => Some(PortStyle::LongFlag),
        "short" => Some(PortStyle::ShortFlag),
        "env" => Some(PortStyle::Env),
        "none" => None,
        _ => return None,
    };
    let default_port: u16 = parts.next()?.parse().ok()?;
    let name = base(parts.next()?);
    let at = argv.iter().position(|w| base(w) == name)?;
    let rest = &argv[at + 1..];
    let env = env_port.and_then(|p| p.trim().parse::<u16>().ok());
    let (port, port_from) = match (last_port(rest), env) {
        (Some(p), _) => (p, PortSource::Argv),
        (None, Some(p)) => (Some(p), PortSource::Env),
        _ => (Some(default_port), PortSource::Default),
    };
    let style = style.filter(|_| port.is_some());
    let mut key = vec![name.clone()];
    key.extend(strip_ports(rest));
    Some((
        DevCommand {
            framework: format!("test {name}"),
            port,
            port_from,
            style,
            launcher: Launcher::Direct,
            script: None,
        },
        key,
    ))
}

fn resolve(argv: &[String], cwd: &Path) -> Option<Resolved> {
    let first = program(argv.first()?);
    let launcher = match first.as_str() {
        "npm" => Launcher::Npm,
        "pnpm" => Launcher::Pnpm,
        "yarn" => Launcher::Yarn,
        "bun" => Launcher::Bun,
        _ => Launcher::Direct,
    };
    if launcher == Launcher::Direct {
        // `npx vite` is `vite`.
        let skip = usize::from(PACKAGE_RUNNERS.contains(&first.as_str()));
        let tokens: Vec<String> = argv[skip..].to_vec();
        if tokens.is_empty() {
            return None;
        }
        return Some(Resolved {
            launcher,
            script: None,
            tokens,
            extra: Vec::new(),
            inline_port: None,
        });
    }
    let rest = &argv[1..];
    let mut i = rest.iter().position(|w| !w.starts_with('-'))?;
    // `bun ./node_modules/.bin/vite` runs a file, not a script.
    if launcher == Launcher::Bun && rest[i].contains('/') {
        return Some(Resolved {
            launcher: Launcher::Direct,
            script: None,
            tokens: argv.to_vec(),
            extra: Vec::new(),
            inline_port: None,
        });
    }
    let is_run = matches!(rest[i].as_str(), "run" | "run-script");
    if is_run {
        i += 1 + rest[i + 1..].iter().position(|w| !w.starts_with('-'))?;
    } else if launcher == Launcher::Npm && rest[i] != "start" {
        return None;
    }
    let script = rest[i].clone();
    let extra = rest[i + 1..].to_vec();
    let pkg = std::fs::read_to_string(cwd.join("package.json")).ok()?;
    let pkg: serde_json::Value = serde_json::from_str(&pkg).ok()?;
    let text = pkg["scripts"][&script].as_str()?;
    if ["&&", "||", ";", "|", "$(", "`", ">", "<"]
        .iter()
        .any(|s| text.contains(s))
    {
        return None;
    }
    let mut words: Vec<&str> = text.split_whitespace().collect();
    let mut inline_port = None;
    // Leading `K=V` words (and `cross-env` / `env`) set the environment.
    while let Some(w) = words.first().copied() {
        if matches!(w, "cross-env" | "env") {
            words.remove(0);
        } else if let Some((k, v)) = w.split_once('=').filter(|(k, _)| is_ident(k)) {
            if k == "PORT" {
                inline_port = v.parse().ok();
            }
            words.remove(0);
        } else {
            break;
        }
    }
    // A script's `npx vite` is `vite` too.
    if words
        .first()
        .is_some_and(|w| PACKAGE_RUNNERS.contains(&program(w).as_str()))
    {
        words.remove(0);
    }
    if words.is_empty() {
        return None;
    }
    Some(Resolved {
        launcher,
        script: Some(script),
        tokens: words.into_iter().map(String::from).collect(),
        extra,
        inline_port,
    })
}

fn is_ident(k: &str) -> bool {
    !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// One port flag in a word list.
struct Hit {
    idx: usize,
    joined: bool,
    port: Option<u16>,
}

/// Every `--port N`, `--port=N`, `-p N` and `-p=N`.
fn scan(words: &[String]) -> Vec<Hit> {
    let mut hits = Vec::new();
    for (idx, w) in words.iter().enumerate() {
        if w == "--port" || w == "-p" {
            let port = words.get(idx + 1).and_then(|v| v.parse().ok());
            hits.push(Hit {
                idx,
                joined: false,
                port,
            });
        } else if let Some(v) = w.strip_prefix("--port=").or_else(|| w.strip_prefix("-p=")) {
            hits.push(Hit {
                idx,
                joined: true,
                port: v.parse().ok(),
            });
        }
    }
    hits
}

/// The port the last port flag of a word list sets: `None` without a port
/// flag, `Some(None)` when its value is not a number (`--port $PORT`).
fn last_port(words: &[String]) -> Option<Option<u16>> {
    scan(words).last().map(|h| h.port)
}

/// Replace the value of every port flag; true when there was one.
fn replace_ports(words: &mut [String], port: &str) -> bool {
    let hits: Vec<Hit> = scan(words)
        .into_iter()
        .filter(|h| h.port.is_some())
        .collect();
    for h in &hits {
        if h.joined {
            let flag = words[h.idx]
                .split('=')
                .next()
                .unwrap_or("--port")
                .to_string();
            words[h.idx] = format!("{flag}={port}");
        } else {
            words[h.idx + 1] = port.to_string();
        }
    }
    !hits.is_empty()
}

/// The words without their port flags and values (a value btp cannot read,
/// such as `$PORT`, included).
fn strip_ports(words: &[String]) -> Vec<String> {
    let mut drop = vec![false; words.len()];
    for h in scan(words) {
        drop[h.idx] = true;
        if !h.joined && words.get(h.idx + 1).is_some_and(|w| !w.starts_with('-')) {
            drop[h.idx + 1] = true;
        }
    }
    words
        .iter()
        .zip(drop)
        .filter(|(_, d)| !d)
        .map(|(w, _)| w.clone())
        .collect()
}

fn base(w: &str) -> String {
    w.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(w)
        .to_string()
}

/// A word's basename without a JavaScript extension.
fn program(w: &str) -> String {
    let b = base(w);
    [".js", ".mjs", ".cjs"]
        .iter()
        .find_map(|ext| b.strip_suffix(ext))
        .unwrap_or(&b)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    fn dir_with_scripts(scripts: &str) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        fs::write(
            d.path().join("package.json"),
            format!(r#"{{"scripts":{scripts}}}"#),
        )
        .unwrap();
        d
    }

    fn rec(a: &[&str]) -> Option<DevCommand> {
        recognize(&v(a), tempfile::tempdir().unwrap().path(), None)
    }

    #[test]
    fn vite_default_5173() {
        let d = rec(&["vite"]).unwrap();
        assert_eq!(d.framework, "Vite");
        assert_eq!(d.port, Some(5173));
        assert_eq!(d.port_from, PortSource::Default);
        assert_eq!(d.style, Some(PortStyle::LongFlag));
        assert_eq!(d.launcher, Launcher::Direct);
    }

    #[test]
    fn defaults_table() {
        let cases: &[(&[&str], &str, u16)] = &[
            (&["next", "dev"], "Next.js", 3000),
            (&["astro", "dev"], "Astro", 4321),
            (&["expo", "start"], "Expo", 8081),
            (&["wrangler", "dev"], "wrangler", 8787),
            (&["svelte-kit", "dev"], "SvelteKit", 5173),
            (&["remix", "dev"], "Remix", 3000),
            (&["nuxt", "dev"], "Nuxt", 3000),
            (&["eleventy", "--serve"], "Eleventy", 8080),
        ];
        for (argv, fw, port) in cases {
            let d = rec(argv).unwrap_or_else(|| panic!("{argv:?}"));
            assert_eq!(
                (d.framework.as_str(), d.port),
                (*fw, Some(*port)),
                "{argv:?}"
            );
        }
        assert_eq!(
            rec(&["next", "dev"]).unwrap().style,
            Some(PortStyle::ShortFlag)
        );
        assert_eq!(rec(&["npx", "vite"]).unwrap().launcher, Launcher::Direct);
    }

    #[test]
    fn non_serving_subcommands_are_none() {
        for a in [
            &["next", "build"][..],
            &["vite", "build"],
            &["wrangler", "deploy"],
            &["astro", "build"],
            &["storybook", "build"],
            &["expo", "install"],
            &["eleventy"],
        ] {
            assert!(rec(a).is_none(), "{a:?}");
        }
        for a in [
            &["next", "start"][..],
            &["vite", "preview"],
            &["storybook", "dev"],
            &["astro", "preview"],
        ] {
            assert!(rec(a).is_some(), "{a:?}");
        }
        // The subcommand also counts when it comes from a script.
        let d = dir_with_scripts(r#"{"build":"next build","dev":"next dev"}"#);
        assert!(recognize(&v(&["npm", "run", "build"]), d.path(), None).is_none());
        assert!(recognize(&v(&["npm", "run", "dev"]), d.path(), None).is_some());
    }

    #[test]
    fn flag_values_are_not_subcommands() {
        let d = rec(&["vite", "--host", "0.0.0.0"]).unwrap();
        assert_eq!((d.framework.as_str(), d.port), ("Vite", Some(5173)));
        assert_eq!(rec(&["next", "-H", "0.0.0.0"]).unwrap().port, Some(3000));
        assert!(rec(&["next", "--hostname", "x"]).is_some());
        assert!(rec(&["vite", "--mode", "staging"]).is_some());
        let d = dir_with_scripts(r#"{"dev":"vite --host 0.0.0.0","x":"cross-env X=1 next dev"}"#);
        assert!(recognize(&v(&["npm", "run", "dev"]), d.path(), None).is_some());
        // cross-env is stripped from a script but not from a direct command,
        // which is therefore not recognized and runs untouched.
        assert!(recognize(&v(&["npm", "run", "x"]), d.path(), None).is_some());
        assert!(rec(&["cross-env", "X=1", "next", "dev"]).is_none());
        assert!(rec(&["next", "build"]).is_none() && rec(&["vite", "build"]).is_none());
    }

    #[test]
    fn argv_port_wins() {
        for a in [
            &["vite", "--port", "4000"][..],
            &["vite", "--port=4000"],
            &["next", "dev", "-p", "4000"],
        ] {
            let d = rec(a).unwrap();
            assert_eq!(
                (d.port, d.port_from),
                (Some(4000), PortSource::Argv),
                "{a:?}"
            );
        }
    }

    #[test]
    fn env_port() {
        let d = recognize(
            &v(&["next", "dev"]),
            tempfile::tempdir().unwrap().path(),
            Some("3500"),
        )
        .unwrap();
        assert_eq!((d.port, d.port_from), (Some(3500), PortSource::Env));
    }

    #[test]
    fn npm_script_resolved() {
        let d = dir_with_scripts(r#"{"dev":"next dev -p 4000"}"#);
        let dev = recognize(&v(&["npm", "run", "dev"]), d.path(), None).unwrap();
        assert_eq!(dev.framework, "Next.js");
        assert_eq!((dev.port, dev.port_from), (Some(4000), PortSource::Script));
        assert_eq!(dev.launcher, Launcher::Npm);
        assert_eq!(dev.script.as_deref(), Some("dev"));
    }

    #[test]
    fn unknown_is_none() {
        let d = dir_with_scripts(r#"{"test":"jest"}"#);
        for a in [&["turbo", "dev"][..], &["make", "serve"], &["npm", "test"]] {
            assert!(recognize(&v(a), d.path(), None).is_none(), "{a:?}");
        }
    }

    #[test]
    fn rewrite_replaces_in_place() {
        let d = tempfile::tempdir().unwrap();
        let check = |a: &[&str], want: &[&str]| {
            let dev = recognize(&v(a), d.path(), None).unwrap();
            let (out, env) = with_port(&v(a), &dev, 3001).unwrap();
            assert_eq!(out, v(want));
            assert!(env.is_empty());
        };
        check(&["vite", "--port", "3000"], &["vite", "--port", "3001"]);
        check(&["vite", "--port=3000"], &["vite", "--port=3001"]);
        check(
            &["next", "dev", "-p", "3000"],
            &["next", "dev", "-p", "3001"],
        );
        check(&["vite"], &["vite", "--port", "3001"]);
        check(&["next", "dev"], &["next", "dev", "-p", "3001"]);
    }

    #[test]
    fn rewrite_through_npm() {
        let d = dir_with_scripts(r#"{"dev":"next dev -p 4000"}"#);
        let argv = v(&["npm", "run", "dev"]);
        let dev = recognize(&argv, d.path(), None).unwrap();
        // Next.js parses with `arg`: a repeated -p overwrites, the last wins,
        // so the appended flag overrides the one in the script.
        let (out, _) = with_port(&argv, &dev, 4001).unwrap();
        assert_eq!(out, v(&["npm", "run", "dev", "--", "-p", "4001"]));
        // A framework not known to take the last flag cannot be rerouted
        // when the script sets the port.
        let d = dir_with_scripts(r#"{"dev":"vite --port 4000"}"#);
        let dev = recognize(&argv, d.path(), None).unwrap();
        assert_eq!(dev.port, Some(4000));
        assert!(dev.style.is_none() && with_port(&argv, &dev, 4001).is_none());
        // A script that sets PORT inline is not reroutable either.
        let d = dir_with_scripts(r#"{"dev":"PORT=4000 next dev"}"#);
        let dev = recognize(&argv, d.path(), None).unwrap();
        assert_eq!((dev.port, dev.port_from), (Some(4000), PortSource::Script));
        assert!(dev.style.is_none());
        // A port flag the user passed after `--` is replaced in place.
        let d = dir_with_scripts(r#"{"dev":"next dev -p 4000"}"#);
        let argv = v(&["npm", "run", "dev", "--", "-p", "5000"]);
        let dev = recognize(&argv, d.path(), None).unwrap();
        assert_eq!((dev.port, dev.port_from), (Some(5000), PortSource::Argv));
        let (out, _) = with_port(&argv, &dev, 5001).unwrap();
        assert_eq!(out, v(&["npm", "run", "dev", "--", "-p", "5001"]));
    }

    #[test]
    fn rewrite_pnpm_yarn_bun() {
        let d = dir_with_scripts(r#"{"dev":"vite"}"#);
        let check = |a: &[&str], want: &[&str]| {
            let dev = recognize(&v(a), d.path(), None).unwrap();
            let (out, _) = with_port(&v(a), &dev, 3001).unwrap();
            assert_eq!(out, v(want));
        };
        check(&["pnpm", "dev"], &["pnpm", "dev", "--", "--port", "3001"]);
        check(
            &["bun", "run", "dev"],
            &["bun", "run", "dev", "--", "--port", "3001"],
        );
        check(&["yarn", "dev"], &["yarn", "dev", "--port", "3001"]);
    }

    #[test]
    fn rewrite_env_style() {
        let argv = v(&["remix-serve", "build/index.js"]);
        let dev = recognize(&argv, tempfile::tempdir().unwrap().path(), None).unwrap();
        assert_eq!(dev.style, Some(PortStyle::Env));
        let (out, env) = with_port(&argv, &dev, 3001).unwrap();
        assert_eq!(out, argv);
        assert_eq!(env, vec![("PORT".to_string(), "3001".to_string())]);
    }

    #[test]
    fn same_key_ignores_port() {
        let d = dir_with_scripts(r#"{"dev":"vite"}"#);
        let key = |a: &[&str]| same_command_key(&v(a), d.path());
        assert!(key(&["vite"]).is_some());
        assert_eq!(key(&["vite", "--port", "3001"]), key(&["vite"]));
        assert_eq!(key(&["npm", "run", "dev"]), key(&["vite"]));
        assert_eq!(
            key(&["npm", "run", "dev", "--", "--port=1"]),
            key(&["vite"])
        );
        assert_ne!(key(&["vite", "--host"]), key(&["vite"]));
        assert!(key(&["make", "serve"]).is_none());
    }

    #[test]
    fn a_framework_named_by_an_argument_is_not_a_server() {
        for a in [
            &["ls", "node_modules/.bin/vite"][..],
            &["rm", "node_modules/.bin/vite"],
            &["cat", "./node_modules/.bin/next"],
            &["git", "add", "node_modules/.bin/vite"],
            &["npx", "rimraf", "node_modules/.bin/vite"],
            &["npx", "prettier", "--write", "node_modules/.bin/next"],
            &["echo", "vite"],
            &["ls", "svelte-kit"],
            &["node", "build.js", "node_modules/.bin/vite"],
            &["node"],
        ] {
            assert!(rec(a).is_none(), "{a:?}");
        }
        // The same rule for the first word of a package.json script.
        let d = dir_with_scripts(
            r#"{"clean":"rimraf node_modules/.bin/vite","fmt":"prettier --write node_modules/.bin/next"}"#,
        );
        for s in ["clean", "fmt"] {
            assert!(
                recognize(&v(&["npm", "run", s]), d.path(), None).is_none(),
                "{s}"
            );
        }
    }

    #[test]
    fn the_program_word_names_the_framework() {
        let cases: &[(&[&str], &str)] = &[
            (&["vite"], "Vite"),
            (&["npx", "vite"], "Vite"),
            (&["bunx", "vite"], "Vite"),
            (&["./node_modules/.bin/vite"], "Vite"),
            (&["node", "node_modules/.bin/next", "dev"], "Next.js"),
            (&["node", "node_modules/vite/bin/vite.js"], "Vite"),
            (&["node", "--inspect", "node_modules/.bin/vite"], "Vite"),
            (&["bun", "node_modules/.bin/vite"], "Vite"),
            (&["nuxi", "dev"], "Nuxt"),
            (&["remix-serve", "build/index.js"], "Remix"),
            (&["start-storybook"], "Storybook"),
        ];
        for (argv, fw) in cases {
            let d = rec(argv).unwrap_or_else(|| panic!("{argv:?}"));
            assert_eq!(d.framework, *fw, "{argv:?}");
        }
        let d = dir_with_scripts(
            r#"{"dev":"vite","x":"npx vite","n":"node node_modules/.bin/next dev"}"#,
        );
        for (s, fw) in [("dev", "Vite"), ("x", "Vite"), ("n", "Next.js")] {
            let dev = recognize(&v(&["npm", "run", s]), d.path(), None).unwrap();
            assert_eq!(dev.framework, fw, "{s}");
        }
        // A server seen running as `node .../.bin/vite` is the same command.
        assert_eq!(
            same_command_key(&v(&["node", "/p/node_modules/.bin/vite"]), d.path()),
            same_command_key(&v(&["vite"]), d.path())
        );
    }

    #[test]
    fn port_env_only_for_frameworks_that_read_it() {
        let d = tempfile::tempdir().unwrap();
        let at = |a: &[&str]| recognize(&v(a), d.path(), Some("3999")).unwrap();
        for a in [
            &["next", "dev"][..],
            &["nuxt", "dev"],
            &["remix-serve", "b"],
        ] {
            let dev = at(a);
            assert_eq!(
                (dev.port, dev.port_from),
                (Some(3999), PortSource::Env),
                "{a:?}"
            );
        }
        for (a, port) in [
            (&["vite"][..], 5173),
            (&["astro", "dev"], 4321),
            (&["wrangler", "dev"], 8787),
            (&["expo", "start"], 8081),
            (&["storybook", "dev"], 6006),
            (&["svelte-kit", "dev"], 5173),
            (&["remix", "vite:dev"], 3000),
            (&["eleventy", "--serve"], 8080),
        ] {
            let dev = at(a);
            assert_eq!(
                (dev.port, dev.port_from),
                (Some(port), PortSource::Default),
                "{a:?}"
            );
            assert!(dev.style.is_some(), "{a:?}");
        }
        // An inline `PORT=` in a script only counts for those too.
        let s = dir_with_scripts(r#"{"dev":"PORT=4000 vite"}"#);
        let argv = v(&["npm", "run", "dev"]);
        let dev = recognize(&argv, s.path(), None).unwrap();
        assert_eq!((dev.port, dev.port_from), (Some(5173), PortSource::Default));
        let (out, env) = with_port(&argv, &dev, 5174).unwrap();
        assert_eq!(out, v(&["npm", "run", "dev", "--", "--port", "5174"]));
        assert!(env.is_empty());
    }

    #[test]
    fn a_port_flag_btp_cannot_read_leaves_the_port_unknown() {
        let d = dir_with_scripts(
            r#"{"a":"vite --port $PORT","b":"vite --port=$PORT","c":"next dev -p $PORT"}"#,
        );
        for s in ["a", "b", "c"] {
            let argv = v(&["npm", "run", s]);
            let dev = recognize(&argv, d.path(), Some("4000")).unwrap();
            assert_eq!((dev.port, dev.style), (None, None), "{s}");
            assert!(with_port(&argv, &dev, 4001).is_none(), "{s}");
        }
        for a in [
            &["vite", "--port", "x"][..],
            &["vite", "--port=x"],
            &["vite", "--port"],
        ] {
            let dev = rec(a).unwrap();
            assert_eq!((dev.port, dev.style), (None, None), "{a:?}");
        }
        // A later readable flag wins as before.
        let dev = rec(&["vite", "--port", "x", "--port", "4000"]).unwrap();
        assert_eq!(dev.port, Some(4000));
    }

    #[test]
    fn test_entry_stands_for_a_framework() {
        let argv = v(&["/usr/bin/python3", "/t/serve.py", "--port", "4100"]);
        let (dev, key) = test_entry("serve.py:8000:long", &argv, None).unwrap();
        assert_eq!((dev.port, dev.port_from), (Some(4100), PortSource::Argv));
        assert_eq!(dev.style, Some(PortStyle::LongFlag));
        assert_eq!(key, v(&["serve.py"]));
        // As `btp run` sees it: the same key, whatever the interpreter.
        let (_, k2) = test_entry("serve.py:8000:long", &v(&["/x/serve.py"]), None).unwrap();
        assert_eq!(k2, key);
        let (dev, _) = test_entry("serve.py:8000:env", &v(&["serve.py"]), Some("4200")).unwrap();
        assert_eq!((dev.port, dev.style), (Some(4200), Some(PortStyle::Env)));
        let (dev, _) = test_entry("serve.py:8000:none", &v(&["serve.py"]), None).unwrap();
        assert_eq!((dev.port, dev.style), (Some(8000), None));
        assert!(test_entry("serve.py:8000:long", &v(&["vite"]), None).is_none());
        assert!(test_entry("serve.py:x:long", &v(&["serve.py"]), None).is_none());
        assert!(test_entry("serve.py:8000:odd", &v(&["serve.py"]), None).is_none());
    }
}
