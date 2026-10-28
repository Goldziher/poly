//! The original repository's git context, handed to hooks that run in the
//! staged snapshot.
//!
//! The snapshot (ADR 0019) is a plain directory in the per-user cache, not a git
//! checkout. A hook that enumerates the index — `git ls-files` feeding a
//! whole-tree secret scan, `git cat-file` reading a staged blob the snapshot
//! dropped — would otherwise find no repository at all, or resolve git's
//! relative `GIT_INDEX_FILE=.git/index` against the snapshot. So every hook in a
//! staged run gets the context git itself gives a hook in a linked worktree:
//!
//! - `GIT_DIR` — the repository's absolute git dir (`.git/worktrees/<name>` in a
//!   linked worktree, never an assumed `<root>/.git`);
//! - `GIT_INDEX_FILE` — the absolute path of the index the in-flight commit is
//!   building: the inherited value when git set one (`git commit <paths>` and
//!   `-a` use an alternate index), resolved against the repository root as the
//!   snapshot's own `checkout-index` resolved it, else `<git dir>/index`;
//! - `GIT_OPTIONAL_LOCKS=0` — so `git status` / `git diff` do not refresh the
//!   index's stat cache, which would write the snapshot's stat data into the
//!   commit's index.
//!
//! With `GIT_DIR` set and no `GIT_WORK_TREE`, git treats the hook's working
//! directory — the snapshot — as the work tree, so `git diff` / `git status`
//! compare the staged bytes against the index rather than the live worktree.
//! An inherited `GIT_WORK_TREE` is removed for the same reason. None is ever
//! exported: with one set, a snapshot-local `git init` records it as the
//! repository's `core.worktree`, re-pointing the real checkout at a cache
//! directory. Without it, that `git init` is a harmless re-initialization.
//!
//! A variable the hook's own `env` sets wins over all of these.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use tracing::warn;

use crate::git::{self, git_cmd, path_from_git_bytes};
use crate::process::Cmd;

const GIT_DIR: &str = "GIT_DIR";
const GIT_INDEX_FILE: &str = "GIT_INDEX_FILE";
const GIT_WORK_TREE: &str = "GIT_WORK_TREE";
const GIT_OPTIONAL_LOCKS: &str = "GIT_OPTIONAL_LOCKS";

/// The original repository and active index, resolved once per staged run.
#[derive(Debug)]
pub(super) struct GitContext {
    git_dir: PathBuf,
    index_file: PathBuf,
}

impl GitContext {
    /// Resolve the context for the repository at `root`, honoring an inherited
    /// `GIT_INDEX_FILE`.
    ///
    /// A failure is logged and yields `None`: the hooks then run as they did
    /// before this context existed, which is no worse than a hook that never
    /// asks git anything.
    pub(super) fn resolve(root: &Path) -> Option<Self> {
        Self::try_resolve(root, std::env::var_os(GIT_INDEX_FILE))
            .inspect_err(|error| warn!(%error, "could not resolve the git context for isolated hooks"))
            .ok()
    }

    fn try_resolve(root: &Path, inherited_index: Option<OsString>) -> Result<Self, git::Error> {
        let output = git_cmd("resolve git dir")?
            .current_dir(root)
            .arg("rev-parse")
            .arg("--absolute-git-dir")
            .check(true)
            .output()?;
        let git_dir = path_from_git_bytes(output.stdout.trim_ascii())?;
        let index_file = index_file(root, &git_dir, inherited_index);
        Ok(Self { git_dir, index_file })
    }

    /// Export the context to a hook's command, leaving any variable the hook's
    /// own `env` sets alone.
    pub(super) fn export(&self, cmd: &mut Cmd, hook_env: &BTreeMap<String, String>) {
        let unset = |key: &str| !hook_env.contains_key(key);
        if unset(GIT_DIR) {
            cmd.env(GIT_DIR, &self.git_dir);
        }
        if unset(GIT_INDEX_FILE) {
            cmd.env(GIT_INDEX_FILE, &self.index_file);
        }
        if unset(GIT_WORK_TREE) {
            cmd.env_remove(GIT_WORK_TREE);
        }
        if unset(GIT_OPTIONAL_LOCKS) {
            cmd.env(GIT_OPTIONAL_LOCKS, "0");
        }
    }
}

/// The absolute index path: the inherited `GIT_INDEX_FILE` when set (relative
/// to `root`, where git ran the hook), else the git dir's own `index`.
fn index_file(root: &Path, git_dir: &Path, inherited: Option<OsString>) -> PathBuf {
    match inherited.filter(|value| !value.is_empty()).map(PathBuf::from) {
        Some(path) if path.is_absolute() => path,
        Some(path) => root.join(path),
        None => git_dir.join("index"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absent_or_empty_inherited_index_falls_back_to_the_git_dirs_own() {
        let root = Path::new("/repo");
        let git_dir = Path::new("/repo/.git/worktrees/wt");
        assert_eq!(index_file(root, git_dir, None), git_dir.join("index"));
        assert_eq!(index_file(root, git_dir, Some(OsString::new())), git_dir.join("index"));
    }

    #[test]
    fn a_relative_inherited_index_resolves_against_the_repository_root() {
        let root = Path::new("/repo");
        let inherited = Some(OsString::from(".git/index"));
        assert_eq!(
            index_file(root, Path::new("/repo/.git"), inherited),
            root.join(".git/index")
        );
    }

    #[test]
    fn an_absolute_inherited_index_is_kept() {
        let alternate = std::env::temp_dir().join("next-index-1.lock");
        let inherited = Some(alternate.clone().into_os_string());
        assert_eq!(
            index_file(Path::new("/repo"), Path::new("/repo/.git"), inherited),
            alternate
        );
    }

    #[test]
    fn the_hooks_own_env_wins() {
        let context = GitContext {
            git_dir: PathBuf::from("/repo/.git"),
            index_file: PathBuf::from("/repo/.git/index"),
        };
        let mut cmd = Cmd::new("true", "probe");
        let hook_env = BTreeMap::from([(GIT_DIR.to_string(), "/custom".to_string())]);
        context.export(&mut cmd, &hook_env);
        let envs: BTreeMap<_, _> = cmd.get_envs().collect();
        assert_eq!(envs.get(std::ffi::OsStr::new(GIT_DIR)), None);
        assert_eq!(
            envs.get(std::ffi::OsStr::new(GIT_INDEX_FILE)),
            Some(&Some(std::ffi::OsStr::new("/repo/.git/index")))
        );
        assert_eq!(envs.get(std::ffi::OsStr::new(GIT_WORK_TREE)), Some(&None));
        assert_eq!(
            envs.get(std::ffi::OsStr::new(GIT_OPTIONAL_LOCKS)),
            Some(&Some(std::ffi::OsStr::new("0")))
        );
    }
}
