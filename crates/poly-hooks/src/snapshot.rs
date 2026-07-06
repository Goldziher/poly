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
//! # Submodules
//!
//! `git checkout-index` writes only blob entries, so a submodule gitlink leaves
//! *no* content in the snapshot. A compile hook that reaches into a submodule
//! (e.g. a test that `include_bytes!`es a fixture from one) would then fail to
//! build in the sandbox even though the real tree compiles. To close that gap,
//! each populated submodule is exposed in the snapshot as a **symlink into the
//! live worktree's submodule directory**: a submodule's files are not part of the
//! parent repo's staged commit (only its pinned gitlink is), so linking to the
//! real checkout is both correct — the parent's hooks never lint the submodule's
//! own sources — and cheap, avoiding a copy of a potentially large fixture tree.
//!
//! # Persistent, incremental cache
//!
//! The snapshot is a **persistent cache** at a stable path outside the repo
//! (`<platform-cache>/poly/<repo-key>/staged`), not a throwaway per-run
//! directory. Each run *refreshes it in place* so every tool's native
//! incremental cache — cargo's `target/`, `.mypy_cache`, tsc's build-info —
//! persists across runs and stays warm:
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
//! run, and it lives outside the repo so it is never committed. Purge it like
//! any cache (`poly cache clean`, or remove the per-user cache dir). Single-writer
//! is assumed, matching the result cache's posture; concurrent `poly hooks` runs
//! on one repo are not locked yet.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tracing::debug;

use crate::git;

/// Directory name for the snapshot under the per-repo cache dir
/// (`<platform-cache>/poly/<repo-key>/staged`).
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

    /// The per-user cache directory (which holds the snapshot) could not be resolved.
    #[error("could not resolve the poly cache directory: {0}")]
    CacheDir(String),
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
    /// Lives at `<platform-cache>/poly/<repo-key>/staged`, outside the repo tree.
    /// The first call materializes the whole staged tree; later calls only touch
    /// what changed (see the module docs).
    pub fn create(root: &Path) -> Result<Self, Error> {
        let cache_dir = poly_cache::repo_cache_dir(root).map_err(|e| Error::CacheDir(e.to_string()))?;
        Self::create_in(&cache_dir, root)
    }

    /// Create or refresh the snapshot under `cache_dir/staged`. Separated from
    /// [`Self::create`] so tests can target an isolated cache dir rather than the
    /// real per-user cache home.
    fn create_in(cache_dir: &Path, root: &Path) -> Result<Self, Error> {
        let dir = cache_dir.join(SNAPSHOT_SUBDIR);
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

    materialize_submodules(root, dir)?;

    write_manifest(dir, &staged)?;
    Ok(())
}

/// Expose each populated submodule in the snapshot as a symlink into the live
/// worktree, so whole-workspace compile hooks can resolve files inside it (see
/// the module docs). An uninitialized submodule (empty worktree directory) is
/// skipped — there is nothing to link and the real build would fail on it too.
fn materialize_submodules(root: &Path, dir: &Path) -> Result<(), Error> {
    for subpath in git::list_submodule_gitlinks(root)? {
        let source = root.join(&subpath);
        if !is_populated_dir(&source) {
            debug!(submodule = %subpath.display(), "skipping uninitialized submodule");
            continue;
        }
        // Canonicalize so the link target is absolute and cwd-independent.
        let target = std::fs::canonicalize(&source).unwrap_or(source);
        ensure_symlink(&target, &dir.join(&subpath))?;
    }
    Ok(())
}

/// Whether `path` is a directory holding at least one entry — i.e. a checked-out,
/// non-empty submodule (an uninitialized submodule is an empty directory).
fn is_populated_dir(path: &Path) -> bool {
    std::fs::read_dir(path).is_ok_and(|mut entries| entries.next().is_some())
}

/// Ensure `link` is a symlink to `target`. Idempotent: an already-correct symlink
/// is left untouched (stable mtime keeps compilers warm); any other existing
/// entry — a stale symlink or an empty `checkout-index` directory — is replaced.
fn ensure_symlink(target: &Path, link: &Path) -> Result<(), Error> {
    match std::fs::read_link(link) {
        Ok(existing) if existing == target => return Ok(()),
        Ok(_) => std::fs::remove_file(link)?,
        Err(_) => {
            // Not a symlink: remove whatever is there (if anything) before linking.
            if let Ok(meta) = std::fs::symlink_metadata(link) {
                if meta.is_dir() {
                    std::fs::remove_dir_all(link)?;
                } else {
                    std::fs::remove_file(link)?;
                }
            }
        }
    }
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent)?;
    }
    symlink_dir(target, link)?;
    Ok(())
}

/// Create a directory symlink at `link` pointing to `target` (platform-specific).
#[cfg(unix)]
fn symlink_dir(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

/// Create a directory symlink at `link` pointing to `target` (platform-specific).
#[cfg(windows)]
fn symlink_dir(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(target, link)
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
        let cache = TempDir::new().expect("cache home");
        let repo = tmp.path();
        init(repo);
        std::fs::write(repo.join("committed.txt"), "staged\n").unwrap();
        std::fs::write(repo.join("unstaged.txt"), "v1\n").unwrap();
        git(repo, &["add", "committed.txt", "unstaged.txt"]);
        std::fs::write(repo.join("unstaged.txt"), "v1\nDIRTY\n").unwrap();
        std::fs::write(repo.join("untracked.txt"), "nope\n").unwrap();

        let snap = StagedSnapshot::create_in(cache.path(), repo).expect("snapshot");

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
        let cache = TempDir::new().expect("cache home");
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

        let snap = StagedSnapshot::create_in(cache.path(), repo).expect("snapshot");

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
        let cache = TempDir::new().expect("cache home");
        let repo = tmp.path();
        init(repo);
        std::fs::write(repo.join("a.rs"), "fn main() {}\n").unwrap();
        git(repo, &["add", "a.rs"]);

        let snap = StagedSnapshot::create_in(cache.path(), repo).expect("first");
        let first = std::fs::metadata(snap.path().join("a.rs")).unwrap().modified().unwrap();

        StagedSnapshot::create_in(cache.path(), repo).expect("refresh");
        let second = std::fs::metadata(snap.path().join("a.rs")).unwrap().modified().unwrap();

        assert_eq!(first, second, "unchanged staged OID must not be rewritten on refresh");
    }

    #[test]
    fn changed_staged_oid_is_rematerialized() {
        // When the staged content changes, the snapshot must pick it up.
        let tmp = TempDir::new().expect("tmp repo");
        let cache = TempDir::new().expect("cache home");
        let repo = tmp.path();
        init(repo);
        std::fs::write(repo.join("a.rs"), "// v1\n").unwrap();
        git(repo, &["add", "a.rs"]);
        let snap = StagedSnapshot::create_in(cache.path(), repo).expect("first");
        assert_eq!(std::fs::read_to_string(snap.path().join("a.rs")).unwrap(), "// v1\n");

        std::fs::write(repo.join("a.rs"), "// v2 changed\n").unwrap();
        git(repo, &["add", "a.rs"]);
        StagedSnapshot::create_in(cache.path(), repo).expect("refresh");
        assert_eq!(
            std::fs::read_to_string(snap.path().join("a.rs")).unwrap(),
            "// v2 changed\n",
            "a newly-staged OID must be re-materialized"
        );
    }

    #[test]
    fn snapshot_exposes_submodule_content_via_symlink() {
        // Regression: `git checkout-index` never materializes a submodule's files,
        // so a compile hook that reaches into a submodule (e.g. `include_bytes!` of
        // a fixture) failed in the sandbox though the real tree compiles. The
        // snapshot must expose the submodule as a symlink into the live worktree.
        let tmp = TempDir::new().expect("tmp");
        let cache = TempDir::new().expect("cache home");
        let root = tmp.path();

        // A standalone repo to embed as a submodule, holding a compile-time fixture.
        let sub = root.join("subrepo_src");
        std::fs::create_dir_all(sub.join("fixtures")).unwrap();
        init(&sub);
        std::fs::write(sub.join("fixtures/data.bin"), b"FIXTURE").unwrap();
        git(&sub, &["add", "."]);
        git(&sub, &["commit", "-q", "-m", "fixture"]);

        // Parent repo with the submodule added at `vendor`. Local-path submodules
        // require `protocol.file.allow=always` (tightened since CVE-2022-39253).
        let parent = root.join("parent");
        std::fs::create_dir_all(&parent).unwrap();
        init(&parent);
        git(
            &parent,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                sub.to_str().unwrap(),
                "vendor",
            ],
        );
        std::fs::write(parent.join("main.rs"), "fn main() {}\n").unwrap();
        git(&parent, &["add", "."]);

        let snap = StagedSnapshot::create_in(cache.path(), &parent).expect("snapshot");

        // The submodule fixture must resolve through the snapshot...
        let via_snapshot = snap.path().join("vendor/fixtures/data.bin");
        assert!(
            via_snapshot.exists(),
            "submodule fixture must resolve through the snapshot"
        );
        assert_eq!(std::fs::read(&via_snapshot).unwrap(), b"FIXTURE");
        // ...and it must be a symlink into the worktree, not an expensive copy.
        assert!(
            std::fs::symlink_metadata(snap.path().join("vendor"))
                .unwrap()
                .file_type()
                .is_symlink(),
            "submodule must be exposed as a symlink, not copied"
        );

        // Refresh is idempotent — the symlink survives a second run.
        StagedSnapshot::create_in(cache.path(), &parent).expect("refresh");
        assert_eq!(std::fs::read(&via_snapshot).unwrap(), b"FIXTURE");
    }

    #[test]
    fn refresh_prunes_files_that_left_the_tree_but_keeps_tool_caches() {
        let tmp = TempDir::new().expect("tmp repo");
        let cache = TempDir::new().expect("cache home");
        let repo = tmp.path();
        init(repo);
        std::fs::write(repo.join("keep.rs"), "a\n").unwrap();
        std::fs::write(repo.join("gone.rs"), "b\n").unwrap();
        git(repo, &["add", "keep.rs", "gone.rs"]);
        git(repo, &["commit", "-q", "-m", "init"]);
        let snap = StagedSnapshot::create_in(cache.path(), repo).expect("first");

        // A tool writes a cache artifact into the snapshot (untracked).
        std::fs::create_dir_all(snap.path().join("target")).unwrap();
        std::fs::write(snap.path().join("target/cache.bin"), "artifact").unwrap();

        // Remove a tracked file, then refresh.
        git(repo, &["rm", "-q", "gone.rs"]);
        StagedSnapshot::create_in(cache.path(), repo).expect("refresh");

        assert!(snap.path().join("keep.rs").exists(), "still-tracked file remains");
        assert!(!snap.path().join("gone.rs").exists(), "untracked file is pruned");
        assert!(
            snap.path().join("target/cache.bin").exists(),
            "tool cache must survive the prune"
        );
    }
}
