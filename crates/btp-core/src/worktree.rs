use crate::labels;
use crate::model::ServerKey;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// The nearest ancestor of `cwd` (itself included) holding a `.git` entry,
/// a directory for a main checkout or a file for a linked worktree. Falls
/// back to `cwd` outside any repository.
pub fn worktree_root(cwd: &Path) -> PathBuf {
    cwd.ancestors()
        .find(|dir| dir.join(".git").symlink_metadata().is_ok())
        .unwrap_or(cwd)
        .to_path_buf()
}

/// The branch checked out in `worktree`, read from its `HEAD` (through the
/// `gitdir:` of a linked worktree). A detached HEAD gives its first 7 hex
/// characters. No subprocess.
pub fn git_branch(worktree: &Path) -> Option<String> {
    let git = worktree.join(".git");
    let dir = if git.is_dir() {
        git
    } else {
        let text = std::fs::read_to_string(&git).ok()?;
        let gitdir = text.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
        worktree.join(gitdir)
    };
    let head = std::fs::read_to_string(dir.join("HEAD")).ok()?;
    let head = head.trim();
    match head.strip_prefix("ref: ") {
        Some(r) => {
            let b = r.trim().strip_prefix("refs/heads/").unwrap_or(r.trim());
            (!b.is_empty()).then(|| b.to_string())
        }
        None if head.len() >= 7 && head.chars().all(|c| c.is_ascii_hexdigit()) => {
            Some(head[..7].to_string())
        }
        None => None,
    }
}

/// How long a worktree's branch is trusted before `HEAD` is read again.
const BRANCH_TTL_MS: u64 = 30_000;

/// Framework and package name per server (read once) and branch per
/// worktree (re-read after 30 s), for the menu rows and detail chips.
#[derive(Default)]
pub struct MetaCache {
    servers: HashMap<ServerKey, (Option<String>, Option<String>)>,
    branches: HashMap<PathBuf, (u64, Option<String>)>,
}

impl MetaCache {
    /// `(framework, package name, branch)` of a server. `package.json` is
    /// read from `cwd`, else from the worktree root.
    pub fn get(
        &mut self,
        key: &ServerKey,
        command: &str,
        cwd: &Path,
        worktree: &Path,
        now_ms: u64,
    ) -> (Option<String>, Option<String>, Option<String>) {
        let (framework, package) = self
            .servers
            .entry(*key)
            .or_insert_with(|| {
                let json = [cwd, worktree]
                    .iter()
                    .find_map(|d| std::fs::read_to_string(d.join("package.json")).ok());
                (
                    labels::framework(command, json.as_deref()),
                    json.as_deref().and_then(labels::package_name),
                )
            })
            .clone();
        let fresh = self
            .branches
            .get(worktree)
            .filter(|(at, _)| now_ms.saturating_sub(*at) < BRANCH_TTL_MS);
        let branch = match fresh {
            Some((_, b)) => b.clone(),
            None => {
                let b = git_branch(worktree);
                self.branches
                    .insert(worktree.to_path_buf(), (now_ms, b.clone()));
                b
            }
        };
        (framework, package, branch)
    }

    /// Forgets the servers not in `live`, and the branches older than their
    /// 30 s (read again on the next `get` anyway).
    pub fn retain(&mut self, live: &HashSet<ServerKey>, now_ms: u64) {
        self.servers.retain(|k, _| live.contains(k));
        self.branches
            .retain(|_, (at, _)| now_ms.saturating_sub(*at) < BRANCH_TTL_MS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn worktree_root_finds_git_file_of_linked_worktree() {
        let d = tempfile::tempdir().unwrap();
        let main = d.path().join("main");
        fs::create_dir_all(main.join(".git")).unwrap();
        let wt = d.path().join("wt");
        fs::create_dir_all(wt.join("apps/web")).unwrap();
        fs::write(wt.join(".git"), "gitdir: ../main/.git/worktrees/wt\n").unwrap();

        assert_eq!(worktree_root(&wt.join("apps/web")), wt);
        assert_eq!(worktree_root(&wt), wt);
        fs::create_dir_all(main.join("src")).unwrap();
        assert_eq!(worktree_root(&main.join("src")), main);
    }

    #[test]
    fn worktree_root_outside_a_repo_is_cwd() {
        let d = tempfile::tempdir().unwrap();
        let cwd = d.path().join("plain/dir");
        fs::create_dir_all(&cwd).unwrap();
        // The temp dir itself is not inside a repository on any sane machine.
        assert_eq!(worktree_root(&cwd), cwd);
    }

    #[test]
    fn git_branch_main_checkout() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join(".git")).unwrap();
        fs::write(d.path().join(".git/HEAD"), "ref: refs/heads/feat/x\n").unwrap();
        assert_eq!(git_branch(d.path()).as_deref(), Some("feat/x"));
    }

    #[test]
    fn git_branch_linked_worktree() {
        let d = tempfile::tempdir().unwrap();
        let abs = d.path().join("repo/.git/worktrees/wt");
        let wt =
            crate::labels::tests::linked_worktree(d.path(), "repo", "wt", abs.to_str().unwrap());
        fs::write(abs.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        assert_eq!(git_branch(&wt).as_deref(), Some("main"));
    }

    #[test]
    fn git_branch_detached_is_short_sha() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join(".git")).unwrap();
        fs::write(
            d.path().join(".git/HEAD"),
            "0123456789abcdef0123456789abcdef01234567\n",
        )
        .unwrap();
        assert_eq!(git_branch(d.path()).as_deref(), Some("0123456"));
    }

    #[test]
    fn git_branch_outside_repo_is_none() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(git_branch(d.path()), None);
    }

    #[test]
    fn meta_cache_reads_package_once_and_branch_for_30_s() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join(".git")).unwrap();
        let head = d.path().join(".git/HEAD");
        fs::write(&head, "ref: refs/heads/a\n").unwrap();
        fs::write(
            d.path().join("package.json"),
            r#"{"name":"web","scripts":{"dev":"vite"}}"#,
        )
        .unwrap();
        let key = ServerKey {
            pid: 1,
            start_time: 1,
        };
        let mut c = MetaCache::default();
        let m = c.get(&key, "npm run dev", d.path(), d.path(), 0);
        assert_eq!(
            m,
            (Some("Vite".into()), Some("web".into()), Some("a".into()))
        );
        fs::write(&head, "ref: refs/heads/b\n").unwrap();
        fs::remove_file(d.path().join("package.json")).unwrap();
        // Within 30 s: cached branch; package never re-read.
        assert_eq!(
            c.get(&key, "npm run dev", d.path(), d.path(), 29_000)
                .2
                .as_deref(),
            Some("a")
        );
        let m = c.get(&key, "npm run dev", d.path(), d.path(), 30_000);
        assert_eq!(
            (m.0.as_deref(), m.1.as_deref(), m.2.as_deref()),
            (Some("Vite"), Some("web"), Some("b"))
        );
    }

    #[test]
    fn meta_cache_retain_forgets_gone_servers_and_stale_branches() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("package.json"), r#"{"name":"web"}"#).unwrap();
        let key = |pid| ServerKey { pid, start_time: 1 };
        let mut c = MetaCache::default();
        c.get(&key(1), "vite", d.path(), d.path(), 0);
        c.get(&key(2), "vite", d.path(), d.path(), 0);
        c.retain(&HashSet::from([key(1)]), 30_000);
        assert_eq!(c.servers.keys().collect::<Vec<_>>(), vec![&key(1)]);
        assert!(c.branches.is_empty());
        // A server forgotten reads its package.json again.
        fs::write(d.path().join("package.json"), r#"{"name":"api"}"#).unwrap();
        assert_eq!(
            c.get(&key(2), "vite", d.path(), d.path(), 30_000)
                .1
                .as_deref(),
            Some("api")
        );
    }
}
