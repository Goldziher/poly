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

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tracing::{debug, warn};

use crate::git;

/// Directory name for the snapshot under the per-repo cache dir
/// (`<platform-cache>/poly/<repo-key>/staged`).
const SNAPSHOT_SUBDIR: &str = "staged";

/// Manifest recording the tracked paths materialized last run, so prune removes
/// only files that fell out of the tree — never tool-generated caches.
const MANIFEST_FILE: &str = ".poly-manifest";

/// Manifest placeholder for a stat field that could not be read when the record
/// was written. It never compares equal to a real stat, so the path is
/// re-materialized on the next refresh while still taking part in the prune.
const UNKNOWN_STAT: &str = "-";

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
    if is_symlink_to(link, target) {
        return Ok(());
    }
    remove_existing_entry(link)?;
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent)?;
    }
    symlink_dir(target, link)?;
    Ok(())
}

/// Whether `link` is already a symlink that resolves to `target`.
fn is_symlink_to(link: &Path, target: &Path) -> bool {
    let is_symlink = std::fs::symlink_metadata(link).is_ok_and(|meta| meta.file_type().is_symlink());
    is_symlink
        && matches!(
            (dunce::canonicalize(link), dunce::canonicalize(target)),
            (Ok(resolved), Ok(want)) if resolved == want
        )
}

/// Remove whatever currently occupies `link` — a symlink of any kind, a real
/// directory, or a file — so a fresh symlink can replace it. Absent entries are
/// a no-op.
///
/// A **directory** symlink on Windows must be removed with `remove_dir`, not
/// `remove_file` (which fails on a directory reparse point); a real directory (a
/// leftover empty `checkout-index` dir) is removed recursively.
fn remove_existing_entry(link: &Path) -> Result<(), Error> {
    let Ok(meta) = std::fs::symlink_metadata(link) else {
        return Ok(());
    };
    if meta.file_type().is_symlink() {
        remove_symlink(link)?;
    } else if meta.is_dir() {
        std::fs::remove_dir_all(link)?;
    } else {
        std::fs::remove_file(link)?;
    }
    Ok(())
}

/// Remove a symlink entry (not its target). On Unix `remove_file` unlinks it;
/// on Windows a directory symlink needs `remove_dir`, with a `remove_file`
/// fallback for a file symlink.
#[cfg(unix)]
fn remove_symlink(link: &Path) -> std::io::Result<()> {
    std::fs::remove_file(link)
}

#[cfg(windows)]
fn remove_symlink(link: &Path) -> std::io::Result<()> {
    std::fs::remove_dir(link).or_else(|_| std::fs::remove_file(link))
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

/// What the manifest records for one materialized path: the index OID it was
/// written from plus the `(size, mtime)` observed immediately afterwards.
///
/// The stat is `None` when it could not be read at write time, which never
/// matches a later observation and so forces a re-materialization.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Record {
    oid: String,
    stat: Option<Stat>,
}

/// Size and modification time of a materialized snapshot file — the fingerprint
/// that detects a write by anything other than `git checkout-index`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stat {
    size: u64,
    mtime_nanos: u128,
}

impl Stat {
    /// Read the fingerprint of `path`, or `None` when it is absent or its mtime
    /// is unrepresentable. `symlink_metadata` is used so a materialized symlink
    /// is fingerprinted as itself rather than as its target.
    fn read(path: &Path) -> Option<Self> {
        let meta = std::fs::symlink_metadata(path).ok()?;
        let mtime = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
        Some(Self {
            size: meta.len(),
            mtime_nanos: mtime.as_nanos(),
        })
    }
}

/// Whether the snapshot copy of `entry` can be left untouched: the index OID is
/// unchanged **and** the file on disk is still byte-for-byte the one we wrote
/// for that OID, as far as its `(size, mtime)` can attest.
fn is_up_to_date(dir: &Path, entry: &git::StagedEntry, previous: &HashMap<PathBuf, Record>) -> bool {
    let Some(record) = previous.get(&entry.path) else {
        return false;
    };
    record.oid == entry.oid && record.stat.is_some() && record.stat == Stat::read(&dir.join(&entry.path))
}

/// Remove snapshot files from the previous manifest that are no longer staged.
/// Restricting deletion to the manifest means tool caches written into the
/// snapshot (`target/`, `.mypy_cache`, …) are never touched.
fn prune_stale(dir: &Path, staged: &[git::StagedEntry], previous: &HashMap<PathBuf, Record>) {
    let current: std::collections::HashSet<&PathBuf> = staged.iter().map(|entry| &entry.path).collect();
    for path in previous.keys() {
        if !current.contains(path) {
            let _ = std::fs::remove_file(dir.join(path));
        }
    }
}

/// Read the previous manifest into a path → [`Record`] map (NUL-separated
/// `<oid> <size> <mtime> <path>` records). An absent or unreadable manifest
/// yields an empty map, so everything is re-materialized once — the safe
/// direction.
fn read_manifest(dir: &Path) -> HashMap<PathBuf, Record> {
    std::fs::read(dir.join(MANIFEST_FILE))
        .map(|bytes| {
            bytes
                .split(|&byte| byte == 0)
                .filter(|slice| !slice.is_empty())
                .filter_map(parse_manifest_record)
                .collect()
        })
        .unwrap_or_default()
}

/// Parse one `<oid> <size> <mtime> <path>` manifest record. The leading fields
/// are space-free, so the path is whatever follows the third space and is taken
/// verbatim (it may itself contain spaces).
///
/// A record written by an older poly carries no stat fields (`<oid> <path>`); it
/// is accepted with `stat: None` so an upgrade keeps the prune ledger and merely
/// re-materializes every path once.
fn parse_manifest_record(record: &[u8]) -> Option<(PathBuf, Record)> {
    let (oid, rest) = split_field(record)?;
    let oid = std::str::from_utf8(oid).ok()?.to_string();
    let (stat, path_bytes) = parse_stat_fields(rest).unwrap_or((None, rest));
    let path = git::path_from_git_bytes(path_bytes).ok()?;
    Some((path, Record { oid, stat }))
}

/// Split the leading space-delimited field off `record`, returning it and the
/// remainder. `None` when the record has no space (so no path can follow).
fn split_field(record: &[u8]) -> Option<(&[u8], &[u8])> {
    let space = record.iter().position(|&byte| byte == b' ')?;
    Some((&record[..space], &record[space + 1..]))
}

/// Parse the `<size> <mtime>` fields, returning them with the remaining path
/// bytes. `None` when `rest` does not start with two such fields — i.e. it is a
/// legacy stat-less record whose path begins right here.
fn parse_stat_fields(rest: &[u8]) -> Option<(Option<Stat>, &[u8])> {
    let (size, rest) = split_field(rest)?;
    let (mtime, rest) = split_field(rest)?;
    let size = std::str::from_utf8(size).ok()?;
    let mtime = std::str::from_utf8(mtime).ok()?;
    if (size, mtime) == (UNKNOWN_STAT, UNKNOWN_STAT) {
        return Some((None, rest));
    }
    let stat = Stat {
        size: size.parse().ok()?,
        mtime_nanos: mtime.parse().ok()?,
    };
    Some((Some(stat), rest))
}

/// Write the manifest for the currently-staged paths, fingerprinting each
/// materialized file as it goes (NUL-separated `<oid> <size> <mtime> <path>`
/// records). A file whose stat cannot be read is recorded with [`UNKNOWN_STAT`]
/// placeholders: it stays in the prune ledger but is re-materialized next run.
fn write_manifest(dir: &Path, staged: &[git::StagedEntry]) -> Result<(), Error> {
    let mut bytes = Vec::new();
    for entry in staged {
        let stat = Stat::read(&dir.join(&entry.path)).map_or_else(
            || format!("{UNKNOWN_STAT} {UNKNOWN_STAT}"),
            |stat| format!("{} {}", stat.size, stat.mtime_nanos),
        );
        bytes.extend_from_slice(entry.oid.as_bytes());
        bytes.push(b' ');
        bytes.extend_from_slice(stat.as_bytes());
        bytes.push(b' ');
        bytes.extend_from_slice(path_to_git_bytes(&entry.path).as_ref());
        bytes.push(0);
    }
    std::fs::write(dir.join(MANIFEST_FILE), bytes)?;
    Ok(())
}

/// Encode a repo-relative path for the manifest, byte-faithfully on unix so a
/// non-UTF-8 path round-trips through [`git::path_from_git_bytes`] instead of
/// being lossily mangled (which would re-materialize it on every run).
#[cfg(unix)]
fn path_to_git_bytes(path: &Path) -> std::borrow::Cow<'_, [u8]> {
    use std::os::unix::ffi::OsStrExt as _;

    std::borrow::Cow::Borrowed(path.as_os_str().as_bytes())
}

#[cfg(not(unix))]
fn path_to_git_bytes(path: &Path) -> std::borrow::Cow<'_, [u8]> {
    match path.to_string_lossy() {
        std::borrow::Cow::Borrowed(text) => std::borrow::Cow::Borrowed(text.as_bytes()),
        std::borrow::Cow::Owned(text) => std::borrow::Cow::Owned(text.into_bytes()),
    }
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
    fn manifest_records_round_trip_including_paths_with_spaces() {
        let parsed = parse_manifest_record(b"abc123 42 7 dir/a file.rs").expect("parse");
        assert_eq!(parsed.0, PathBuf::from("dir/a file.rs"));
        assert_eq!(
            parsed.1,
            Record {
                oid: "abc123".to_string(),
                stat: Some(Stat {
                    size: 42,
                    mtime_nanos: 7
                }),
            }
        );

        let unknown = parse_manifest_record(b"abc123 - - a.rs").expect("parse unknown stat");
        assert_eq!(unknown.1.stat, None, "an unknown stat must never match a real one");

        // A manifest written by an older poly has no stat fields.
        let legacy = parse_manifest_record(b"abc123 a.rs").expect("parse legacy");
        assert_eq!(legacy.0, PathBuf::from("a.rs"));
        assert_eq!(legacy.1.stat, None);
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
