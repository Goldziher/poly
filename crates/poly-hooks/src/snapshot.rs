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
//! # Symlink entries
//!
//! `git checkout-index` reproduces a `120000` entry as a real symlink, and the
//! blob it is built from is an arbitrary target string — including an absolute
//! path outside the repository (`~/.ssh/authorized_keys`, a CI credentials
//! file). Anyone who can get a commit staged chooses that string, and the
//! commit gate runs by default, so a materialized escaping link turns any hook
//! that writes to its matched files into an arbitrary file write.
//!
//! Each refresh therefore **removes** any materialized symlink whose target
//! does not lexically resolve inside the snapshot ([`sanitize_symlinks`]).
//! Escaping links are removed rather than all symlinks being skipped: a
//! *relative, in-tree* link is ordinary repository content that a workspace
//! build may legitimately follow (a shared config, a fixture), and dropping
//! those would break real builds to defend against a target that never leaves
//! the tree. The submodule links this module creates itself are deliberately
//! outside that rule — they point at the live worktree's submodule checkout by
//! design (above) and are created after the sanitizing pass.
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
//!   last snapshot, its snapshot copy is missing, or that copy's `(size, mtime)`
//!   no longer matches what was observed right after it was written — tracked by
//!   a manifest of `path → (OID, size, mtime)`. Unchanged paths are left
//!   untouched, so their mtime is stable across runs and a compiler sees
//!   "unchanged" and does not rebuild.
//!
//!   The stat check is what keeps the snapshot honest against writers other than
//!   `git checkout-index`: a workspace hook that *rewrites* files in place (a
//!   formatter, `cargo clippy --fix`) mutates the snapshot without changing any
//!   index OID, and an OID-only comparison would then keep serving the mutated
//!   content on every later run — silently gating on bytes that are not what is
//!   staged. Comparing the stat we recorded at write time detects that and
//!   re-materializes from the index. (Like git's own stat cache, a same-size
//!   rewrite landing inside the filesystem's mtime granularity can still evade
//!   detection; the OID check bounds the exposure to that narrow race.)
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
//!
//! # Module layout
//!
//! This file owns the lifecycle — [`StagedSnapshot`], `refresh`, and the
//! symlink sanitizing that must run immediately after the checkout. The two
//! concerns a refresh delegates to live beside it: `manifest` (the incremental
//! ledger that decides what to re-materialize and what to prune) and
//! `submodule` (linking populated submodules into the snapshot).

use std::path::{Path, PathBuf};

use tracing::{debug, warn};

use crate::git;

mod manifest;
mod submodule;

use self::manifest::{is_up_to_date, prune_stale, read_manifest, write_manifest};
use self::submodule::materialize_submodules;

/// Directory name for the snapshot under the per-repo cache dir
/// (`<platform-cache>/poly/<repo-key>/staged`).
const SNAPSHOT_SUBDIR: &str = "staged";

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

    /// Create or refresh the snapshot under `cache_dir/staged`, for a caller
    /// that chooses its own cache directory.
    ///
    /// Separated from [`Self::create`] so a test can target an isolated cache
    /// dir rather than the real per-user cache home — and so it exercises this
    /// exact materialization path rather than a hand-rolled `git checkout-index`
    /// that would miss, for instance, the symlink sanitizing above.
    pub fn create_in(cache_dir: &Path, root: &Path) -> Result<Self, Error> {
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

    let mut to_checkout: Vec<PathBuf> = Vec::new();
    for entry in &staged {
        if !is_up_to_date(dir, entry, &previous) {
            to_checkout.push(entry.path.clone());
        }
    }
    git::checkout_index_paths(root, dir, &to_checkout)?;
    sanitize_symlinks(dir, &staged);

    materialize_submodules(root, dir)?;

    write_manifest(dir, &staged)?;
    Ok(())
}

/// Remove every materialized symlink whose target does not resolve inside the
/// snapshot (see the module docs). Runs on every refresh, not just on the run
/// that materialized the entry, so a link that survived an earlier poly is
/// cleaned up too.
fn sanitize_symlinks(dir: &Path, staged: &[git::StagedEntry]) {
    for entry in staged.iter().filter(|entry| entry.is_symlink) {
        let link = dir.join(&entry.path);
        let Ok(target) = std::fs::read_link(&link) else {
            continue;
        };
        if resolves_inside_tree(&entry.path, &target) {
            continue;
        }
        warn!(
            path = %entry.path.display(),
            target = %target.display(),
            "removing a staged symlink whose target escapes the snapshot"
        );
        // `remove_file` unlinks the symlink itself, never its target.
        let _ = std::fs::remove_file(&link);
    }
}

/// Whether a symlink staged at `link_path` (repo-relative) with content
/// `target` resolves to somewhere inside the tree it was materialized into.
///
/// Purely lexical, and deliberately so: the answer must not depend on what
/// happens to exist on this machine, and a canonicalizing check would itself
/// follow the link it is meant to judge. An absolute target — or one whose `..`
/// components climb past the root — escapes.
fn resolves_inside_tree(link_path: &Path, target: &Path) -> bool {
    use std::path::Component;

    let mut depth = link_path.components().count().saturating_sub(1);
    for component in target.components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => {
                if depth == 0 {
                    return false;
                }
                depth -= 1;
            }
            Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    use super::manifest::MANIFEST_FILE;

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

    /// The exact bytes git would commit for `path` — `git show :<path>` reads
    /// the index blob, which is the ground truth the snapshot must equal.
    fn index_blob(repo: &Path, path: &str) -> Vec<u8> {
        let output = Command::new("git")
            .args(["show", &format!(":{path}")])
            .current_dir(repo)
            .output()
            .expect("run git show");
        assert!(output.status.success(), "git show :{path} failed");
        output.stdout
    }

    fn assert_matches_index(repo: &Path, snapshot: &Path, path: &str, context: &str) {
        assert_eq!(
            std::fs::read(snapshot.join(path)).expect("read snapshot copy"),
            index_blob(repo, path),
            "snapshot must equal the index blob after a refresh ({context})"
        );
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
        let tmp = TempDir::new().expect("tmp repo");
        let cache = TempDir::new().expect("cache home");
        let repo = tmp.path();
        init(repo);
        std::fs::write(repo.join("big.h"), "STAGED\n").unwrap();
        git(repo, &["add", "big.h"]);
        git(repo, &["commit", "-q", "-m", "init"]);
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
    fn refresh_restores_a_snapshot_file_mutated_out_of_band() {
        let tmp = TempDir::new().expect("tmp repo");
        let cache = TempDir::new().expect("cache home");
        let repo = tmp.path();
        init(repo);
        std::fs::write(repo.join("a.rs"), "// staged\n").unwrap();
        git(repo, &["add", "a.rs"]);
        let snap = StagedSnapshot::create_in(cache.path(), repo).expect("first");
        assert_matches_index(repo, snap.path(), "a.rs", "initial materialization");

        // A workspace hook that rewrites files in place (a formatter, `clippy
        // --fix`) mutates the snapshot without touching any index OID. The next
        // refresh must not keep gating on those bytes.
        std::fs::write(snap.path().join("a.rs"), "// MUTATED IN SNAPSHOT\n").unwrap();

        StagedSnapshot::create_in(cache.path(), repo).expect("refresh");
        assert_matches_index(repo, snap.path(), "a.rs", "after an out-of-band write");
    }

    #[test]
    fn refresh_restores_a_snapshot_file_truncated_out_of_band() {
        let tmp = TempDir::new().expect("tmp repo");
        let cache = TempDir::new().expect("cache home");
        let repo = tmp.path();
        init(repo);
        std::fs::write(repo.join("a.rs"), "// staged\n").unwrap();
        git(repo, &["add", "a.rs"]);
        let snap = StagedSnapshot::create_in(cache.path(), repo).expect("first");

        std::fs::write(snap.path().join("a.rs"), b"").unwrap();
        StagedSnapshot::create_in(cache.path(), repo).expect("refresh");

        assert_matches_index(repo, snap.path(), "a.rs", "after truncation");
    }

    #[test]
    fn snapshot_matches_the_index_across_successive_restages() {
        let tmp = TempDir::new().expect("tmp repo");
        let cache = TempDir::new().expect("cache home");
        let repo = tmp.path();
        init(repo);

        for content in ["// v1\n", "// v2 longer content\n", "// v3\n"] {
            std::fs::write(repo.join("a.rs"), content).unwrap();
            git(repo, &["add", "a.rs"]);
            let snap = StagedSnapshot::create_in(cache.path(), repo).expect("refresh");
            assert_matches_index(repo, snap.path(), "a.rs", content);

            // An unstaged edit on top must never reach the snapshot.
            std::fs::write(repo.join("a.rs"), "// unstaged edit\n").unwrap();
            let snap = StagedSnapshot::create_in(cache.path(), repo).expect("refresh");
            assert_matches_index(repo, snap.path(), "a.rs", "unstaged edit present");
        }
    }

    #[test]
    fn legacy_manifest_still_prunes_files_that_left_the_tree() {
        let tmp = TempDir::new().expect("tmp repo");
        let cache = TempDir::new().expect("cache home");
        let repo = tmp.path();
        init(repo);
        std::fs::write(repo.join("keep.rs"), "a\n").unwrap();
        std::fs::write(repo.join("gone.rs"), "b\n").unwrap();
        git(repo, &["add", "keep.rs", "gone.rs"]);
        git(repo, &["commit", "-q", "-m", "init"]);
        let snap = StagedSnapshot::create_in(cache.path(), repo).expect("first");

        // Rewrite the manifest in the pre-stat format an older poly produced.
        let legacy: Vec<u8> = ["keep.rs", "gone.rs"]
            .iter()
            .flat_map(|name| {
                let mut record = b"0000000000000000000000000000000000000000 ".to_vec();
                record.extend_from_slice(name.as_bytes());
                record.push(0);
                record
            })
            .collect();
        std::fs::write(snap.path().join(MANIFEST_FILE), legacy).unwrap();

        git(repo, &["rm", "-q", "gone.rs"]);
        StagedSnapshot::create_in(cache.path(), repo).expect("refresh");

        assert!(snap.path().join("keep.rs").exists(), "still-tracked file remains");
        assert!(
            !snap.path().join("gone.rs").exists(),
            "a legacy manifest must still serve as the prune ledger"
        );
    }

    #[test]
    fn snapshot_exposes_submodule_content_via_symlink() {
        let tmp = TempDir::new().expect("tmp");
        let cache = TempDir::new().expect("cache home");
        let root = tmp.path();

        let sub = root.join("subrepo_src");
        std::fs::create_dir_all(sub.join("fixtures")).unwrap();
        init(&sub);
        std::fs::write(sub.join("fixtures/data.bin"), b"FIXTURE").unwrap();
        git(&sub, &["add", "."]);
        git(&sub, &["commit", "-q", "-m", "fixture"]);

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

        let via_snapshot = snap.path().join("vendor/fixtures/data.bin");
        assert!(
            via_snapshot.exists(),
            "submodule fixture must resolve through the snapshot"
        );
        assert_eq!(std::fs::read(&via_snapshot).unwrap(), b"FIXTURE");
        assert!(
            std::fs::symlink_metadata(snap.path().join("vendor"))
                .unwrap()
                .file_type()
                .is_symlink(),
            "submodule must be exposed as a symlink, not copied"
        );

        StagedSnapshot::create_in(cache.path(), &parent).expect("refresh");
        assert_eq!(std::fs::read(&via_snapshot).unwrap(), b"FIXTURE");
    }

    #[test]
    fn resolves_inside_tree_judges_targets_lexically() {
        let inside = |link: &str, target: &str| resolves_inside_tree(Path::new(link), Path::new(target));

        assert!(inside("a/link", "sibling.rs"));
        assert!(inside("a/link", "../b/shared.rs"));
        assert!(inside("a/b/link", "../../top.rs"));
        assert!(inside("link", "./nested/file.rs"));

        assert!(!inside("link", "/etc/passwd"), "an absolute target always escapes");
        assert!(!inside("link", "../outside.rs"), "a top-level link may not climb out");
        assert!(!inside("a/link", "../../outside.rs"));
        assert!(!inside("a/b/link", "../../../outside.rs"));
    }

    /// THE ARBITRARY FILE WRITE. A tracked `120000` entry whose blob is an
    /// absolute path outside the repository — chosen by whoever staged it —
    /// would be recreated as a real symlink in the snapshot, and every hook that
    /// rewrites its matched files would then write straight through it to that
    /// path. The snapshot must not hand a hook a door out of the tree.
    #[cfg(unix)]
    #[test]
    fn a_staged_symlink_pointing_outside_the_repository_is_not_materialized() {
        let tmp = TempDir::new().expect("tmp repo");
        let cache = TempDir::new().expect("cache home");
        let outside = TempDir::new().expect("outside the repo");
        let repo = tmp.path();
        init(repo);

        let victim = outside.path().join("authorized_keys");
        std::fs::write(&victim, "ORIGINAL\n").unwrap();
        std::os::unix::fs::symlink(&victim, repo.join("evil.rs")).unwrap();
        git(repo, &["add", "evil.rs"]);

        let snap = StagedSnapshot::create_in(cache.path(), repo).expect("snapshot");

        let materialized = snap.path().join("evil.rs");
        assert!(
            std::fs::symlink_metadata(&materialized).is_err(),
            "an escaping symlink must not exist in the snapshot at all"
        );

        // The proof, in the shape the auditor's PoC used: a hook writing to its
        // matched path inside the snapshot must not reach outside the tree.
        std::fs::write(&materialized, "PWNED\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            "ORIGINAL\n",
            "writing to the snapshot path must never reach the external target"
        );
    }

    /// The complement: a relative link that stays inside the tree is ordinary
    /// repository content a build may follow, and is kept.
    #[cfg(unix)]
    #[test]
    fn a_staged_symlink_pointing_inside_the_repository_is_kept() {
        let tmp = TempDir::new().expect("tmp repo");
        let cache = TempDir::new().expect("cache home");
        let repo = tmp.path();
        init(repo);

        std::fs::create_dir_all(repo.join("pkg")).unwrap();
        std::fs::write(repo.join("shared.toml"), "shared = true\n").unwrap();
        std::os::unix::fs::symlink("../shared.toml", repo.join("pkg/config.toml")).unwrap();
        git(repo, &["add", "."]);

        let snap = StagedSnapshot::create_in(cache.path(), repo).expect("snapshot");

        let link = snap.path().join("pkg/config.toml");
        assert!(
            std::fs::symlink_metadata(&link).expect("link present").is_symlink(),
            "an in-tree relative link is legitimate content and must survive"
        );
        assert_eq!(std::fs::read_to_string(&link).unwrap(), "shared = true\n");
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

        std::fs::create_dir_all(snap.path().join("target")).unwrap();
        std::fs::write(snap.path().join("target/cache.bin"), "artifact").unwrap();

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
