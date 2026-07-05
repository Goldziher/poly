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
//! # Persistent, incremental cache
//!
//! The snapshot is a **persistent, git-ignored cache** at a stable path
//! (`<repo>/.polylint/staged`), not a throwaway per-run directory. Each run
//! *refreshes it in place* so every tool's native incremental cache — cargo's
//! `target/`, `.mypy_cache`, tsc's build-info — persists across runs and stays
//! warm:
//!
//! - Content is always sourced from the **index blob** (`git checkout-index`),
//!   never copied from the worktree. Sourcing from the index is what makes the
//!   snapshot byte-faithful to what a commit would capture: an unstaged worktree
//!   edit can never leak in, regardless of the state of git's stat cache. (An
//!   earlier design copied clean files from the worktree and only checked out
//!   files `git diff-files` flagged as modified — but `diff-files` is stat-based
//!   and can under-report a genuinely-modified file as clean when the index stat
//!   cache is stale, silently leaking the unstaged edit. Sourcing from the index
//!   OID removes that dependency entirely.)
//! - A path is (re)materialized only when its **index OID changed** since the
//!   last snapshot (or its snapshot copy is missing), tracked by a manifest of
//!   `path → OID`. Unchanged paths are left untouched, so their mtime is stable
//!   across runs and a compiler sees "unchanged" and does not rebuild.
//! - Files that are no longer tracked are pruned via the same manifest, so
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

use std::collections::HashMap;
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
    let staged = git::list_staged_entries(root)?;
    let previous = read_manifest(dir);

    prune_stale(dir, &staged, &previous);

    // Content always comes from the index blob (authoritative, stat-independent).
    // A path is re-materialized only when its index OID changed since the last
    // snapshot or its snapshot copy is missing; unchanged paths are left in
    // place so their mtime is stable across runs and compilers stay warm.
    let mut to_checkout: Vec<PathBuf> = Vec::new();
    for entry in &staged {
        let up_to_date = previous.get(&entry.path) == Some(&entry.oid) && snapshot_present(&dir.join(&entry.path));
        if !up_to_date {
            to_checkout.push(entry.path.clone());
        }
    }
    git::checkout_index_paths(root, dir, &to_checkout)?;

    write_manifest(dir, &staged)?;
    Ok(())
}

/// Whether a snapshot entry exists on disk. Uses `symlink_metadata` so a
/// materialized symlink counts as present even if its target is absent.
fn snapshot_present(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// Remove snapshot files from the previous manifest that are no longer staged.
/// Restricting deletion to the manifest means tool caches written into the
/// snapshot (`target/`, `.mypy_cache`, …) are never touched.
fn prune_stale(dir: &Path, staged: &[git::StagedEntry], previous: &HashMap<PathBuf, String>) {
    let current: std::collections::HashSet<&PathBuf> = staged.iter().map(|entry| &entry.path).collect();
    for path in previous.keys() {
        if !current.contains(path) {
            // Best-effort: a file a hook already removed is fine.
            let _ = std::fs::remove_file(dir.join(path));
        }
    }
}

/// Read the previous manifest into a `path → OID` map (NUL-separated
/// `<oid> <path>` records). An absent or unreadable manifest yields an empty map
/// (everything is treated as needing materialization).
fn read_manifest(dir: &Path) -> HashMap<PathBuf, String> {
    std::fs::read(dir.join(MANIFEST_FILE))
        .map(|bytes| {
            bytes
                .split(|&byte| byte == 0)
                .filter(|slice| !slice.is_empty())
                .filter_map(|record| {
                    let space = record.iter().position(|&byte| byte == b' ')?;
                    let oid = std::str::from_utf8(&record[..space]).ok()?.to_string();
                    let path = git::path_from_git_bytes(&record[space + 1..]).ok()?;
                    Some((path, oid))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Write the manifest of currently-staged `path → OID` pairs (NUL-separated
/// `<oid> <path>` records; the OID has no spaces so the first space delimits it).
fn write_manifest(dir: &Path, staged: &[git::StagedEntry]) -> Result<(), Error> {
    let mut bytes = Vec::new();
    for entry in staged {
        bytes.extend_from_slice(entry.oid.as_bytes());
        bytes.push(b' ');
        bytes.extend_from_slice(entry.path.to_string_lossy().as_bytes());
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
    fn snapshot_uses_index_content_even_when_worktree_differs_in_size() {
        // The core guarantee: an unstaged worktree edit — even one `git
        // diff-files` might under-report from a stale stat cache — must never
        // reach the snapshot, because content is sourced from the index OID.
        let tmp = TempDir::new().expect("tmp repo");
        let repo = tmp.path();
        init(repo);
        std::fs::write(repo.join("big.h"), "STAGED\n").unwrap();
        git(repo, &["add", "big.h"]);
        git(repo, &["commit", "-q", "-m", "init"]);
        // A genuine, size-changing unstaged edit (never staged).
        std::fs::write(
            repo.join("big.h"),
            "WORKTREE EDIT that is much longer than the staged blob\n",
        )
        .unwrap();

        let snap = StagedSnapshot::create(repo).expect("snapshot");

        assert_eq!(
            std::fs::read_to_string(snap.path().join("big.h")).unwrap(),
            "STAGED\n",
            "snapshot must hold the staged blob, not the unstaged worktree edit"
        );
    }

    #[test]
    fn unchanged_file_is_not_rematerialized_across_refreshes() {
        // A file whose staged OID is unchanged must be left untouched on refresh
        // (stable mtime) so a compiler's incremental cache stays warm.
        let tmp = TempDir::new().expect("tmp repo");
        let repo = tmp.path();
        init(repo);
        std::fs::write(repo.join("a.rs"), "fn main() {}\n").unwrap();
        git(repo, &["add", "a.rs"]);

        let snap = StagedSnapshot::create(repo).expect("first");
        let first = std::fs::metadata(snap.path().join("a.rs")).unwrap().modified().unwrap();

        StagedSnapshot::create(repo).expect("refresh");
        let second = std::fs::metadata(snap.path().join("a.rs")).unwrap().modified().unwrap();

        assert_eq!(first, second, "unchanged staged OID must not be rewritten on refresh");
    }

    #[test]
    fn changed_staged_oid_is_rematerialized() {
        // When the staged content changes, the snapshot must pick it up.
        let tmp = TempDir::new().expect("tmp repo");
        let repo = tmp.path();
        init(repo);
        std::fs::write(repo.join("a.rs"), "// v1\n").unwrap();
        git(repo, &["add", "a.rs"]);
        let snap = StagedSnapshot::create(repo).expect("first");
        assert_eq!(std::fs::read_to_string(snap.path().join("a.rs")).unwrap(), "// v1\n");

        std::fs::write(repo.join("a.rs"), "// v2 changed\n").unwrap();
        git(repo, &["add", "a.rs"]);
        StagedSnapshot::create(repo).expect("refresh");
        assert_eq!(
            std::fs::read_to_string(snap.path().join("a.rs")).unwrap(),
            "// v2 changed\n",
            "a newly-staged OID must be re-materialized"
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
