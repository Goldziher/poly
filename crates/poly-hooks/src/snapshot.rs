//! Staged-content snapshots for whole-workspace hook isolation.
//!
//! Whole-workspace tools (`cargo clippy`, type checkers like `pyrefly`, `mypy`,
//! `tsc`, …) compile or analyze the entire project, so they cannot be scoped to
//! a staged *file list* the way per-file hooks are. To isolate them to staged
//! content without the destructive worktree mutation that `git stash` / `git
//! checkout -- .` would require, [`StagedSnapshot`] materializes the git
//! **index** into a directory and lets those hooks run there. The snapshot is
//! byte-faithful staged content — untracked files and unstaged worktree edits
//! are absent — and the live worktree is never touched.
//!
//! # Persistent, mtime-faithful cache
//!
//! The snapshot is a **persistent, git-ignored cache** at a stable path
//! (`<repo>/.polylint/staged`), not a throwaway per-run directory. Each run
//! *refreshes it in place* so every tool's native incremental cache — cargo's
//! `target/`, `.mypy_cache`, tsc's build-info — persists across runs and stays
//! warm:
//!
//! - Tracked files whose worktree equals the index are **copied from the
//!   worktree preserving their mtime** (skipped entirely when the snapshot copy
//!   is already up to date), so a compiler sees "unchanged" and does not rebuild.
//! - Only files whose worktree differs from the index (a staged file carrying an
//!   extra unstaged edit) — plus symlinks — are rewritten from the staged blob
//!   via `git checkout-index`.
//! - Files that are no longer tracked are pruned, tracked by a manifest so
//!   tool-generated caches inside the snapshot are never removed.
//!
//! # Cleanup
//!
//! Being a managed cache, it is *not* deleted after every run — that is what
//! keeps incremental caches warm. Instead it is bounded and self-healing: each
//! refresh prunes stale files, a crash mid-refresh is corrected by the next
//! run, and it is git-ignored so it is never committed. Purge it like any cache
//! (`rm -rf .polylint/staged`). Single-writer is assumed, matching the result
//! cache's posture; concurrent `poly hooks` runs on one repo are not locked yet.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use tracing::debug;

use crate::git;

/// Directory name for the snapshot under the repo-local `.polylint/` cache dir.
const SNAPSHOT_SUBDIR: &str = "staged";

/// Manifest recording the tracked paths materialized last run, so prune removes
/// only files that fell out of the tree — never tool-generated caches.
const MANIFEST_FILE: &str = ".poly-manifest";

/// Errors returned while creating or refreshing a [`StagedSnapshot`].
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Filesystem error while materializing the snapshot.
    #[error("staged-snapshot I/O failed: {0}")]
    Io(#[from] std::io::Error),

    /// A `git` invocation failed while resolving or materializing staged content.
    #[error(transparent)]
    Git(#[from] git::Error),
}

/// A non-destructive, persistent copy of the repository's staged content.
///
/// Call [`Self::path`] to get the root to run whole-workspace hooks from.
#[derive(Debug)]
pub struct StagedSnapshot {
    dir: PathBuf,
}

impl StagedSnapshot {
    /// Create or refresh the staged snapshot for the repository at `root`.
    ///
    /// Lives at `<root>/.polylint/staged`. The first call materializes the whole
    /// staged tree; later calls only touch what changed (see the module docs).
    pub fn create(root: &Path) -> Result<Self, Error> {
        let dir = root.join(".polylint").join(SNAPSHOT_SUBDIR);
        std::fs::create_dir_all(&dir)?;
        refresh(root, &dir)?;
        debug!(snapshot = %dir.display(), "refreshed staged snapshot");
        Ok(Self { dir })
    }

    /// The snapshot root — the working directory for whole-workspace hooks.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.dir
    }
}

/// Refresh `dir` so it mirrors the current staged (index) content of `root`.
fn refresh(root: &Path, dir: &Path) -> Result<(), Error> {
    let tracked = git::list_files(root)?;
    let dirty: HashSet<PathBuf> = git::worktree_modified_files(root)?.into_iter().collect();

    prune_stale(dir, &tracked);

    // Files whose worktree copy is not the staged content (an extra unstaged
    // edit) — plus symlinks — are pulled from the staged blob; everything else
    // is copied from the worktree preserving its mtime.
    let mut from_index: Vec<PathBuf> = Vec::new();
    for path in &tracked {
        let src = root.join(path);
        if dirty.contains(path) || is_symlink(&src) {
            from_index.push(path.clone());
        } else {
            copy_preserving_mtime(&src, &dir.join(path))?;
        }
    }
    git::checkout_index_paths(root, dir, &from_index)?;

    write_manifest(dir, &tracked)?;
    Ok(())
}

/// Remove snapshot files listed in the previous manifest that are no longer
/// tracked. Restricting deletion to the manifest means tool caches written into
/// the snapshot (`target/`, `.mypy_cache`, …) are never touched.
fn prune_stale(dir: &Path, tracked: &[PathBuf]) {
    let previous = read_manifest(dir);
    let current: HashSet<&PathBuf> = tracked.iter().collect();
    for path in &previous {
        if !current.contains(path) {
            // Best-effort: a file a hook already removed is fine.
            let _ = std::fs::remove_file(dir.join(path));
        }
    }
}

/// Copy `src` to `dst` and stamp `dst`'s mtime from `src`, unless `dst` already
/// matches `src` in size and mtime (the steady-state fast path — no copy).
fn copy_preserving_mtime(src: &Path, dst: &Path) -> Result<(), Error> {
    let src_meta = std::fs::metadata(src)?;
    let src_mtime = filetime::FileTime::from_last_modification_time(&src_meta);
    if let Ok(dst_meta) = std::fs::metadata(dst) {
        if dst_meta.len() == src_meta.len() && filetime::FileTime::from_last_modification_time(&dst_meta) == src_mtime {
            return Ok(());
        }
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(src, dst)?;
    filetime::set_file_mtime(dst, src_mtime)?;
    Ok(())
}

fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
}

/// Read the previous manifest (NUL-separated repo-relative paths); an absent or
/// unreadable manifest yields an empty set (nothing to prune).
fn read_manifest(dir: &Path) -> HashSet<PathBuf> {
    std::fs::read(dir.join(MANIFEST_FILE))
        .map(|bytes| {
            bytes
                .split(|&byte| byte == 0)
                .filter(|slice| !slice.is_empty())
                .map(|slice| PathBuf::from(String::from_utf8_lossy(slice).into_owned()))
                .collect()
        })
        .unwrap_or_default()
}

/// Write the manifest of currently-tracked paths (NUL-separated).
fn write_manifest(dir: &Path, tracked: &[PathBuf]) -> Result<(), Error> {
    let mut bytes = Vec::new();
    for path in tracked {
        bytes.extend_from_slice(path.to_string_lossy().as_bytes());
        bytes.push(0);
    }
    std::fs::write(dir.join(MANIFEST_FILE), bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn git(repo: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .args(args)
            .current_dir(repo)
            .status()
            .expect("run git")
            .success();
        assert!(ok, "git {args:?} failed");
    }

    fn init(repo: &Path) {
        git(repo, &["init", "-q"]);
        git(repo, &["config", "user.email", "t@t"]);
        git(repo, &["config", "user.name", "t"]);
        git(repo, &["config", "commit.gpgsign", "false"]);
    }

    #[test]
    fn snapshot_contains_staged_not_unstaged_or_untracked() {
        let tmp = TempDir::new().expect("tmp repo");
        let repo = tmp.path();
        init(repo);
        std::fs::write(repo.join("committed.txt"), "staged\n").unwrap();
        std::fs::write(repo.join("unstaged.txt"), "v1\n").unwrap();
        git(repo, &["add", "committed.txt", "unstaged.txt"]);
        std::fs::write(repo.join("unstaged.txt"), "v1\nDIRTY\n").unwrap();
        std::fs::write(repo.join("untracked.txt"), "nope\n").unwrap();

        let snap = StagedSnapshot::create(repo).expect("snapshot");

        assert_eq!(
            std::fs::read_to_string(snap.path().join("committed.txt")).unwrap(),
            "staged\n"
        );
        // The staged blob ("v1"), not the dirty worktree ("v1\nDIRTY").
        assert_eq!(
            std::fs::read_to_string(snap.path().join("unstaged.txt")).unwrap(),
            "v1\n"
        );
        assert!(
            !snap.path().join("untracked.txt").exists(),
            "untracked file must not be in the snapshot"
        );
    }

    #[test]
    fn clean_file_mtime_is_preserved_from_the_worktree() {
        let tmp = TempDir::new().expect("tmp repo");
        let repo = tmp.path();
        init(repo);
        std::fs::write(repo.join("a.rs"), "fn main() {}\n").unwrap();
        git(repo, &["add", "a.rs"]);

        let worktree_mtime =
            filetime::FileTime::from_last_modification_time(&std::fs::metadata(repo.join("a.rs")).unwrap());
        let snap = StagedSnapshot::create(repo).expect("snapshot");
        let snap_mtime =
            filetime::FileTime::from_last_modification_time(&std::fs::metadata(snap.path().join("a.rs")).unwrap());
        assert_eq!(
            snap_mtime, worktree_mtime,
            "unchanged file keeps its worktree mtime so compilers stay warm"
        );
    }

    #[test]
    fn refresh_prunes_files_that_left_the_tree_but_keeps_tool_caches() {
        let tmp = TempDir::new().expect("tmp repo");
        let repo = tmp.path();
        init(repo);
        std::fs::write(repo.join("keep.rs"), "a\n").unwrap();
        std::fs::write(repo.join("gone.rs"), "b\n").unwrap();
        git(repo, &["add", "keep.rs", "gone.rs"]);
        git(repo, &["commit", "-q", "-m", "init"]);
        let snap = StagedSnapshot::create(repo).expect("first");

        // A tool writes a cache artifact into the snapshot (untracked).
        std::fs::create_dir_all(snap.path().join("target")).unwrap();
        std::fs::write(snap.path().join("target/cache.bin"), "artifact").unwrap();

        // Remove a tracked file, then refresh.
        git(repo, &["rm", "-q", "gone.rs"]);
        StagedSnapshot::create(repo).expect("refresh");

        assert!(snap.path().join("keep.rs").exists(), "still-tracked file remains");
        assert!(!snap.path().join("gone.rs").exists(), "untracked file is pruned");
        assert!(
            snap.path().join("target/cache.bin").exists(),
            "tool cache must survive the prune"
        );
    }
}
