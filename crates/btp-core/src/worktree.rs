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
}
