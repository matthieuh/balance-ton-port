//! Human-readable labels for a tracked server: what it is (`tool_label`),
//! where it runs (`location_label`) and which Claude Code session started it
//! (`session_display_name`). Pure functions, shared by the CLI and the app.

use std::path::Path;

/// A readable name for a server from its command line, such as
/// `Expo (Metro)`, `Vite`, `Python http.server` or `Node server.js`.
pub fn tool_label(command: &str) -> String {
    let words: Vec<&str> = command.split_whitespace().collect();
    let Some(&argv0) = words.first() else {
        return String::new();
    };
    // The first program naming a known tool wins, so
    // `node …/node_modules/.bin/expo start` reads as Expo, not Node. A word
    // is a program when it is the first one, follows a launcher (`node`,
    // `npx`, `python`…) or is a path; a plain argument such as the `serve`
    // in `workerd serve` is not.
    let mut program_pos = true;
    for (i, w) in words.iter().enumerate() {
        if is_flag(w) {
            continue;
        }
        // `node …/react-native/cli.js start` runs Metro too.
        let name = match program_name(w).as_str() {
            "cli" if w.contains("/react-native/") => "react-native".to_string(),
            n => n.to_string(),
        };
        if program_pos || w.contains('/') {
            if let Some(label) = known_tool(&name, &words[i + 1..]) {
                return label;
            }
        }
        program_pos = is_launcher(&name);
    }
    let rest = &words[1..];
    match program_name(argv0).as_str() {
        "bun" => with_script("Bun", rest, &["run"]),
        "node" | "nodejs" => with_script("Node", rest, &[]),
        "deno" => with_script("Deno", rest, &["run", "serve", "task"]),
        p if is_python(p) => python_label(rest),
        p => match first_arg(rest) {
            Some(a) => format!("{p} {}", base(a)),
            None => p.to_string(),
        },
    }
}

/// The label for a word that names a known dev server, `rest` being the
/// words after it.
fn known_tool(name: &str, rest: &[&str]) -> Option<String> {
    let next = first_arg(rest);
    let label = match name {
        "expo" => "Expo (Metro)",
        "metro" => "Metro",
        "react-native" => "Metro",
        "vite" => "Vite",
        "astro" => "Astro",
        "next" | "next-server" => "Next.js",
        "wrangler" => {
            return Some(match next {
                Some(sub) => format!("wrangler {sub}"),
                None => "wrangler".to_string(),
            })
        }
        "serve" => "serve",
        "nuxt" | "nuxi" => "Nuxt",
        "remix" | "remix-serve" => "Remix",
        "webpack-dev-server" => "webpack dev server",
        "webpack" if next == Some("serve") => "webpack dev server",
        "storybook" | "start-storybook" => "Storybook",
        "rails" => "Rails",
        "manage.py" if next == Some("runserver") => "Django",
        _ => return None,
    };
    Some(label.to_string())
}

fn python_label(rest: &[&str]) -> String {
    if let Some(i) = rest.iter().position(|w| *w == "-m") {
        return match rest.get(i + 1) {
            Some(&"http.server") => "Python http.server".to_string(),
            Some(m) => format!("Python {m}"),
            None => "Python".to_string(),
        };
    }
    with_script("Python", rest, &[])
}

/// `<runtime> <script basename>`, skipping flags and the given subcommands
/// (`bun run dev` gives `Bun dev`).
fn with_script(runtime: &str, rest: &[&str], subcommands: &[&str]) -> String {
    let script = rest
        .iter()
        .find(|w| !is_flag(w) && !subcommands.contains(w));
    match script {
        Some(s) => format!("{runtime} {}", base(s)),
        None => runtime.to_string(),
    }
}

/// A program that runs the next word as another program or script.
fn is_launcher(p: &str) -> bool {
    matches!(
        p,
        "node"
            | "nodejs"
            | "bun"
            | "bunx"
            | "npx"
            | "pnpm"
            | "pnpx"
            | "yarn"
            | "dlx"
            | "exec"
            | "deno"
            | "ruby"
            | "env"
    ) || is_python(p)
}

fn is_python(p: &str) -> bool {
    p == "python"
        || p.strip_prefix("python")
            .is_some_and(|v| v.chars().all(|c| c.is_ascii_digit() || c == '.'))
}

fn is_flag(w: &str) -> bool {
    w.starts_with('-')
}

fn first_arg<'a>(rest: &[&'a str]) -> Option<&'a str> {
    rest.iter().copied().find(|w| !is_flag(w))
}

/// The last path component of a word.
fn base(w: &str) -> &str {
    match w.trim_end_matches('/').rsplit('/').next() {
        Some(b) if !b.is_empty() => b,
        _ => w,
    }
}

/// A word's basename without a JavaScript extension (`vite.js` is `vite`).
fn program_name(w: &str) -> String {
    let b = base(w);
    [".js", ".mjs", ".cjs"]
        .iter()
        .find_map(|ext| b.strip_suffix(ext))
        .unwrap_or(b)
        .to_string()
}

/// Where a server runs: `<repo> › <worktree dir>` for a linked git worktree,
/// the directory name for a main checkout, otherwise the path with the home
/// directory shortened to `~`.
pub fn location_label(worktree: &Path) -> String {
    location_label_with(worktree, dirs::home_dir().as_deref())
}

/// `location_label` against an explicit home directory.
pub fn location_label_with(worktree: &Path, home: Option<&Path>) -> String {
    let git = worktree.join(".git");
    if git.is_dir() {
        return dir_name(worktree);
    }
    if git.is_file() {
        return match linked_repo_name(worktree, &git) {
            Some(repo) => format!("{repo} \u{203A} {}", dir_name(worktree)),
            None => dir_name(worktree),
        };
    }
    match home.and_then(|h| worktree.strip_prefix(h).ok()) {
        Some(rel) if rel.as_os_str().is_empty() => "~".to_string(),
        Some(rel) => format!("~/{}", rel.display()),
        None => worktree.display().to_string(),
    }
}

/// The repository a linked worktree belongs to, from its `.git` file
/// (`gitdir: <repo>/.git/worktrees/<name>`, or `<repo>.git/worktrees/<name>`
/// for a bare repository).
fn linked_repo_name(worktree: &Path, git_file: &Path) -> Option<String> {
    let text = std::fs::read_to_string(git_file).ok()?;
    let gitdir = text.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
    let gitdir = worktree.join(gitdir);
    let worktrees = gitdir.parent()?;
    if worktrees.file_name()? != "worktrees" {
        return None;
    }
    let git_dir = worktrees.parent()?;
    let name = git_dir.file_name()?.to_string_lossy();
    if name == ".git" {
        Some(
            git_dir
                .parent()?
                .file_name()?
                .to_string_lossy()
                .into_owned(),
        )
    } else {
        Some(name.strip_suffix(".git").unwrap_or(&name).to_string())
    }
}

fn dir_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

/// The session's `name` as Claude Code writes it, or the basename of its
/// working directory when the name is missing or blank.
pub fn session_display_name(name: Option<&str>, cwd: &Path) -> String {
    match name.map(str::trim) {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => dir_name(cwd),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn expo_and_metro() {
        assert_eq!(
            tool_label(
                "node /Users/me/.superset/worktrees/revva/kindly-fork-203818d8/node_modules/.bin/expo start --clear --port 8081"
            ),
            "Expo (Metro)"
        );
        assert_eq!(tool_label("npx expo start"), "Expo (Metro)");
        assert_eq!(
            tool_label("node /repo/node_modules/react-native/cli.js start"),
            "Metro"
        );
        assert_eq!(tool_label("react-native start --port 8082"), "Metro");
    }

    #[test]
    fn js_dev_servers() {
        assert_eq!(
            tool_label("node /repo/node_modules/.bin/vite --port 5173"),
            "Vite"
        );
        assert_eq!(
            tool_label("node /repo/node_modules/vite/bin/vite.js"),
            "Vite"
        );
        assert_eq!(
            tool_label("/Users/me/.bun/bin/bun /repo/node_modules/.bin/astro dev"),
            "Astro"
        );
        assert_eq!(
            tool_label(
                "/opt/homebrew/bin/node /repo/node_modules/next/dist/bin/next dev --turbopack"
            ),
            "Next.js"
        );
        assert_eq!(tool_label("next-server (v15.1.0)"), "Next.js");
        assert_eq!(
            tool_label("node /repo/node_modules/.bin/wrangler dev --port 3010"),
            "wrangler dev"
        );
        assert_eq!(tool_label("npx serve -l 3000 dist"), "serve");
        assert_eq!(tool_label("node /repo/node_modules/.bin/nuxi dev"), "Nuxt");
        assert_eq!(tool_label("nuxt dev"), "Nuxt");
        assert_eq!(
            tool_label("node /repo/node_modules/.bin/remix dev"),
            "Remix"
        );
        assert_eq!(tool_label("remix-serve build/index.js"), "Remix");
        assert_eq!(
            tool_label("node /repo/node_modules/.bin/webpack-dev-server --hot"),
            "webpack dev server"
        );
        assert_eq!(
            tool_label("npx webpack serve --mode development"),
            "webpack dev server"
        );
        assert_eq!(
            tool_label("node /repo/node_modules/.bin/storybook dev -p 6006"),
            "Storybook"
        );
        assert_eq!(tool_label("start-storybook -p 6006"), "Storybook");
    }

    #[test]
    fn python_ruby_django() {
        assert_eq!(
            tool_label("python3 -m http.server 8765"),
            "Python http.server"
        );
        assert_eq!(
            tool_label("/usr/bin/python -m http.server"),
            "Python http.server"
        );
        assert_eq!(
            tool_label("python manage.py runserver 0.0.0.0:8000"),
            "Django"
        );
        assert_eq!(tool_label("python3 -m uvicorn app:app"), "Python uvicorn");
        assert_eq!(tool_label("python3 -u /srv/app.py"), "Python app.py");
        assert_eq!(tool_label("ruby bin/rails server -p 3000"), "Rails");
        assert_eq!(tool_label("bin/rails s"), "Rails");
    }

    #[test]
    fn runtimes_with_a_script() {
        assert_eq!(tool_label("bun run dev"), "Bun dev");
        assert_eq!(
            tool_label("/Users/me/.bun/bin/bun --hot src/index.ts"),
            "Bun index.ts"
        );
        assert_eq!(tool_label("bun run --hot ./server.ts"), "Bun server.ts");
        assert_eq!(tool_label("bun"), "Bun");
        assert_eq!(
            tool_label("node /srv/app/server.js --port 3000"),
            "Node server.js"
        );
        assert_eq!(tool_label("node --inspect dist/main.mjs"), "Node main.mjs");
        assert_eq!(tool_label("/usr/local/bin/node"), "Node");
        assert_eq!(tool_label("deno run -A main.ts"), "Deno main.ts");
        assert_eq!(tool_label("deno task dev"), "Deno dev");
        assert_eq!(tool_label("deno"), "Deno");
    }

    #[test]
    fn fallback_is_argv0_basename_and_first_argument() {
        assert_eq!(
            tool_label("/usr/local/bin/workerd serve --verbose config.capnp"),
            "workerd serve"
        );
        assert_eq!(tool_label("--weird"), "--weird");
        assert_eq!(tool_label("/opt/bin/caddy"), "caddy");
        assert_eq!(tool_label("go run --race ./cmd/api"), "go run");
        assert_eq!(tool_label("   "), "");
    }

    #[test]
    fn known_tool_names_inside_paths_do_not_match() {
        // `expo` is a directory here, not the program.
        assert_eq!(tool_label("node /work/expo/server.js"), "Node server.js");
        // A flag value that looks like a tool.
        assert_eq!(tool_label("caddy --config=/etc/vite"), "caddy");
    }

    fn linked_worktree(root: &Path, repo: &str, wt: &str, gitdir: &str) -> PathBuf {
        fs::create_dir_all(root.join(repo).join(".git/worktrees").join(wt)).unwrap();
        let dir = root.join("trees").join(wt);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(".git"), format!("gitdir: {gitdir}\n")).unwrap();
        dir
    }

    #[test]
    fn linked_worktree_shows_repo_and_dir() {
        let d = tempfile::tempdir().unwrap();
        let abs = d.path().join("revva/.git/worktrees/kindly-fork");
        let wt = linked_worktree(d.path(), "revva", "kindly-fork", abs.to_str().unwrap());
        assert_eq!(location_label_with(&wt, None), "revva \u{203A} kindly-fork");
        // A relative gitdir is resolved against the worktree.
        let wt = linked_worktree(d.path(), "revva", "rel", "../../revva/.git/worktrees/rel");
        assert_eq!(location_label_with(&wt, None), "revva \u{203A} rel");
    }

    #[test]
    fn worktree_of_a_bare_repo_drops_the_git_suffix() {
        let d = tempfile::tempdir().unwrap();
        let wt = d.path().join("wt");
        fs::create_dir_all(&wt).unwrap();
        fs::write(wt.join(".git"), "gitdir: /src/app.git/worktrees/feat\n").unwrap();
        assert_eq!(location_label_with(&wt, None), "app \u{203A} wt");
    }

    #[test]
    fn main_checkout_is_its_dir_name() {
        let d = tempfile::tempdir().unwrap();
        let repo = d.path().join("balance-ton-port");
        fs::create_dir_all(repo.join(".git")).unwrap();
        assert_eq!(location_label_with(&repo, None), "balance-ton-port");
    }

    #[test]
    fn git_file_without_worktrees_is_its_dir_name() {
        let d = tempfile::tempdir().unwrap();
        let sub = d.path().join("vendor/lib");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join(".git"), "gitdir: ../../.git/modules/lib\n").unwrap();
        assert_eq!(location_label_with(&sub, None), "lib");
        fs::write(sub.join(".git"), "garbage").unwrap();
        assert_eq!(location_label_with(&sub, None), "lib");
    }

    #[test]
    fn plain_dir_is_its_path_with_home_shortened() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path().join("home");
        let dir = home.join("scratch/site");
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(location_label_with(&dir, Some(&home)), "~/scratch/site");
        assert_eq!(location_label_with(&home, Some(&home)), "~");
        let other = d.path().join("elsewhere");
        fs::create_dir_all(&other).unwrap();
        assert_eq!(
            location_label_with(&other, Some(&home)),
            other.display().to_string()
        );
        // A sibling whose name only starts like home is not under it.
        let sibling = d.path().join("home2");
        fs::create_dir_all(&sibling).unwrap();
        assert_eq!(
            location_label_with(&sibling, Some(&home)),
            sibling.display().to_string()
        );
        assert_eq!(
            location_label_with(Path::new("/tmp/btp-srv"), None),
            "/tmp/btp-srv"
        );
    }

    #[test]
    fn session_name_falls_back_to_cwd_basename() {
        let cwd = Path::new("/Users/me/Workspace/my-app");
        assert_eq!(
            session_display_name(Some("my-app-1d"), cwd),
            "my-app-1d"
        );
        assert_eq!(session_display_name(Some("  "), cwd), "my-app");
        assert_eq!(session_display_name(None, cwd), "my-app");
        assert_eq!(session_display_name(None, Path::new("/")), "/");
    }
}
