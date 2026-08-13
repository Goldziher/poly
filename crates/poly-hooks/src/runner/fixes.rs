//! Detecting the files a hook rewrote, and getting a staged-run fix back into
//! the working tree without destroying unstaged work.
//!
//! Two questions the sequential boundary in [`super::run_hooks`] needs answered
//! after a hook exits:
//!
//! 1. *Did it rewrite its own inputs?* — [`Fingerprints`]. This gates
//!    `stage_fixed` re-staging and the result-cache store (an entry may only be
//!    kept for a run that changed nothing, or the next hit would replay a pass
//!    that was only reached by fixing).
//! 2. *Where does the fix go?* — [`write_back`]. Under a staged run the hook
//!    rewrote the **snapshot**, not the worktree, so the fix has to be carried
//!    across — and only where doing so cannot lose work.
//!
//! Detection is content-based rather than stat-based on purpose: a formatter
//! that rewrites a file to the same length within one filesystem mtime tick is
//! not exotic, and a missed rewrite here means either a lost fix or a cached
//! "passed" for content that never passed on its own. The same reasoning governs
//! the write-back gate: it compares the worktree's **bytes** against the index
//! blob rather than asking `git diff-files`, whose stat-based answer can call a
//! genuinely-modified file clean (see [`crate::git::staged_blob`]).
//!
//! # Writing into someone else's tree
//!
//! [`write_back`] is the only place poly writes to a file the user did not ask
//! it to format, so it is written defensively:
//!
//! - **Never through a symlink.** A tracked symlink entry (`120000`) carries an
//!   arbitrary target — including an absolute path outside the repository — and
//!   both `File::open` and `fs::copy` follow links. Writing a hook's output
//!   through one is an arbitrary file write; a symlink destination is withheld,
//!   never followed.
//! - **Never through an escaping path.** Index paths are joined onto the
//!   worktree root, so `..` or an absolute component is rejected rather than
//!   trusted to git's own `verify_path`.
//! - **Never partially.** The write is a sibling temp file plus a rename, so an
//!   interrupt (Ctrl-C still reaches poly — see [`crate::supervise`]) cannot
//!   leave the author's source truncated.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::git;
use crate::model::{HookStatus, WithheldFix, WithheldReason};

use super::HookRun;

/// A blake3 digest of one file's bytes.
type ContentHash = [u8; blake3::OUT_LEN];

/// Content fingerprints of a hook's files, captured before the hook runs so an
/// in-place rewrite can be detected afterwards.
pub(super) struct Fingerprints {
    /// One entry per captured path; `None` when the file could not be read.
    entries: Vec<(PathBuf, Option<ContentHash>)>,
}

impl Fingerprints {
    /// An empty capture — for hooks whose outcome does not depend on whether
    /// they rewrote anything, so no file is ever read for them.
    pub(super) fn none() -> Self {
        Self { entries: Vec::new() }
    }

    /// Fingerprint each repo-relative path in `paths` beneath `root`.
    ///
    /// An unreadable path (a staged deletion, a permissions error) fingerprints
    /// as `None`, and so does anything that is not a regular file — notably a
    /// symlink, which [`hash_file`] refuses to read *through*. `None` compares
    /// equal to a later `None` — a file absent before and after was not touched —
    /// and unequal to any hash, so a file the hook created counts as modified.
    pub(super) fn capture(root: &Path, paths: &[PathBuf]) -> Self {
        Self {
            entries: paths
                .iter()
                .map(|path| (path.clone(), hash_file(&root.join(path))))
                .collect(),
        }
    }

    /// The captured paths whose content differs from what was captured.
    pub(super) fn modified(&self, root: &Path) -> Vec<PathBuf> {
        self.entries
            .iter()
            .filter(|(path, before)| hash_file(&root.join(path)) != *before)
            .map(|(path, _)| path.clone())
            .collect()
    }
}

/// Hash a file's bytes, or `None` when it is not a regular file or cannot be
/// read.
///
/// The [`is_regular_file`] guard is load-bearing: `File::open` follows symlinks,
/// so hashing a tracked symlink would read whatever it points at — potentially
/// far outside the repository — and report it as this path's content.
fn hash_file(path: &Path) -> Option<ContentHash> {
    if !is_regular_file(path) {
        return None;
    }
    let mut hasher = blake3::Hasher::new();
    let mut file = fs_err::File::open(path).ok()?;
    std::io::copy(&mut file, &mut hasher).ok()?;
    Some(*hasher.finalize().as_bytes())
}

/// Whether `path` is a regular file — not a symlink, directory, device, or
/// absent.
///
/// Checked with `symlink_metadata`, which does **not** follow the final
/// component, so a symlink answers `false` instead of being judged by its
/// target. (Between this check and the `open`/`rename` that follows it, a local
/// attacker could still swap the entry; closing that fully needs `O_NOFOLLOW`
/// and is out of scope here. The check removes the standing, unprivileged
/// vector — a symlink committed to the repository — which is the one poly's
/// own behaviour creates.)
fn is_regular_file(path: &Path) -> bool {
    fs_err::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_file())
}

/// Land the rewrites made by a whole priority group, updating each hook's
/// outcome.
///
/// This runs once per group, not once per hook, because the hooks in a group
/// run **concurrently**: two of them can touch the same file, and a per-hook
/// pass would let the first one's write make the second one's write-back look
/// unsafe. Deciding each path exactly once — on the worktree state as it was
/// before the group's rewrites were carried across — keeps the result
/// independent of which hook happened to finish first.
///
/// `stage_fixed[i]` corresponds to `runs[i]`. Only a **passing** hook's
/// rewrites are landed: a failing hook's half-finished output must never reach
/// the index.
pub(super) fn land_group_rewrites(
    root: &Path,
    snapshot: Option<&Path>,
    stage_fixed: &[bool],
    runs: &mut [HookRun],
) -> anyhow::Result<()> {
    let rewritten: BTreeSet<PathBuf> = runs
        .iter()
        .filter(|run| passed(run))
        .flat_map(|run| run.modified.iter().cloned())
        .collect();
    if rewritten.is_empty() {
        return Ok(());
    }

    // A worktree run needs no transfer — the hook already wrote the very files
    // the commit will take — so `stage_fixed` just stages them.
    let Some(snapshot) = snapshot else {
        for (index, run) in runs.iter_mut().enumerate() {
            if stage_fixed[index] && passed(run) && !run.modified.is_empty() {
                git::add(root, &run.modified)?;
                run.outcome.files_modified = true;
            }
        }
        return Ok(());
    };

    let paths: Vec<PathBuf> = rewritten.into_iter().collect();
    let withheld: BTreeMap<PathBuf, WithheldReason> = write_back(root, snapshot, &paths)?
        .into_iter()
        .map(|fix| (fix.path, fix.reason))
        .collect();

    let mut to_stage: BTreeSet<PathBuf> = BTreeSet::new();
    for (index, run) in runs.iter_mut().enumerate() {
        if !passed(run) {
            continue;
        }
        let (mine_withheld, mine_applied): (Vec<PathBuf>, Vec<PathBuf>) = run
            .modified
            .iter()
            .cloned()
            .partition(|path| withheld.contains_key(path));
        // The reason travels with the path: the report has to say *why* this
        // fix did not land, and the three refusals ask the reader for three
        // different things (see `WithheldReason`).
        let mine_withheld: Vec<WithheldFix> = mine_withheld
            .into_iter()
            .map(|path| {
                let reason = withheld[&path];
                WithheldFix::new(path, reason)
            })
            .collect();
        if stage_fixed[index] {
            run.outcome.files_modified = !mine_applied.is_empty();
            to_stage.extend(mine_applied);
        }
        // Reported whether or not the hook stages its fixes. Without
        // `stage_fixed` the fix was only ever going to land unstaged, but a fix
        // that reached neither the index nor the worktree has vanished — and a
        // rewrite silently swallowed by isolation is exactly what the caller
        // must not have to guess at.
        if !mine_withheld.is_empty() {
            run.outcome.status = HookStatus::FixWithheld(mine_withheld);
        }
    }
    let staged: Vec<PathBuf> = to_stage.into_iter().collect();
    git::add(root, &staged)?;
    Ok(())
}

/// Whether a hook exited 0 — the only state whose rewrites may be landed.
fn passed(run: &HookRun) -> bool {
    matches!(run.outcome.status, HookStatus::Passed)
}

/// Carry each rewrite made in `snapshot` back into the worktree at `root`,
/// returning the paths it **refused** to touch. Staging is the caller's
/// decision (`stage_fixed`); this only moves bytes.
///
/// The rewrite was computed from **staged** bytes, which makes the write safe in
/// exactly one case: the worktree copy is byte-identical to the index, so it
/// holds nothing the rewrite has not already seen. Then writing it reproduces
/// the pre-isolation result exactly — the file the author sees is the fixed one.
///
/// Where the worktree copy differs, the author is holding unstaged work. Writing
/// the staged-derived fix over it would destroy that work outright, and staging
/// the file would commit hunks they deliberately left out. Neither is a call
/// this runner may make on their behalf, so the path is withheld and the caller
/// decides what that means for the hook's verdict.
///
/// "Differs" is decided on content (see [`withhold_reason`]), never on `git
/// diff-files`: a stat-based answer that wrongly says *clean* would have this
/// function overwrite real unstaged work with no warning.
///
/// Each refusal is returned **with its reason**. The unstaged-work case and the
/// symlink / escaping-path case are not the same event and must not reach the
/// reader as the same sentence.
fn write_back(root: &Path, snapshot: &Path, modified: &[PathBuf]) -> anyhow::Result<Vec<WithheldFix>> {
    let mut withheld = Vec::new();
    for path in modified {
        if let Some(reason) = withhold_reason(root, path)? {
            withheld.push(WithheldFix::new(path.clone(), reason));
            continue;
        }
        // The source is inside poly's own snapshot, but it is read with the same
        // guard: `git checkout-index` reproduces symlink entries, so "a path in
        // the snapshot" is not by itself a promise of a regular file.
        let source = snapshot.join(path);
        let Some(bytes) = read_regular_file(&source) else {
            withheld.push(WithheldFix::new(path.clone(), WithheldReason::SnapshotUnreadable));
            continue;
        };
        write_atomic(&root.join(path), &bytes)?;
    }
    Ok(withheld)
}

/// Why the fix for `path` may **not** be written into the worktree at `root`, or
/// `None` when it may.
///
/// The checks are ordered so nothing opens or follows the destination before its
/// type is known:
///
/// 1. the path must not escape `root` (`..`, an absolute component);
/// 2. the worktree entry must be a **regular file** — a symlink is withheld, not
///    followed, or a hook's output would be written to whatever it points at
///    (an absolute target outside the repository is a valid index blob, so this
///    is an arbitrary-file-write vector, not a hypothetical);
/// 3. its bytes must equal the staged blob, so the rewrite has already seen
///    everything the file holds.
///
/// A symlink is distinguished from every other non-regular entry with
/// `symlink_metadata` — which does not follow the final component — because the
/// two produce different advice: one is a refusal to follow a link, the other is
/// a destination that simply is not there.
fn withhold_reason(root: &Path, path: &Path) -> anyhow::Result<Option<WithheldReason>> {
    if !git::is_safe_relative_path(path) {
        return Ok(Some(WithheldReason::PathEscapesRepository));
    }
    let destination = root.join(path);
    match fs_err::symlink_metadata(&destination).map(|meta| meta.file_type()) {
        Ok(kind) if kind.is_file() => {}
        Ok(kind) if kind.is_symlink() => return Ok(Some(WithheldReason::WorktreeIsSymlink)),
        _ => return Ok(Some(WithheldReason::WorktreeNotRegularFile)),
    }
    if worktree_matches_staged(root, path)? {
        Ok(None)
    } else {
        Ok(Some(WithheldReason::UnstagedChanges))
    }
}

/// Whether the worktree copy of `path` is byte-identical to the staged blob —
/// the content-based replacement for `git diff-files`.
///
/// A path with no staged blob, a destination that is not a regular file, and an
/// unreadable destination all answer `false`: every one of them means the
/// worktree holds something the staged-content run did not see.
pub(super) fn worktree_matches_staged(root: &Path, path: &Path) -> anyhow::Result<bool> {
    let Some(staged) = git::staged_blob(root, path)? else {
        return Ok(false);
    };
    Ok(hash_file(&root.join(path)) == Some(*blake3::hash(&staged).as_bytes()))
}

/// Read `path` only if it is a regular file, so a symlink is never followed.
fn read_regular_file(path: &Path) -> Option<Vec<u8>> {
    if !is_regular_file(path) {
        return None;
    }
    fs_err::read(path).ok()
}

/// Write `contents` to `path` atomically: a sibling temp file, then a rename,
/// carrying the destination's original permissions across.
///
/// This mirrors `write_atomic` in `poly-core`'s runner (`poly-hooks` does not
/// depend on `poly-core`, so the technique is duplicated rather than imported —
/// keep the two recognisably the same). The reason is the same in both places:
/// a direct write truncates the destination first, so an interrupt between the
/// truncate and the last byte leaves the author's source file destroyed with
/// nothing to roll back to. `rename` is atomic, and it replaces the directory
/// entry rather than writing through it, so it cannot traverse a symlink either.
fn write_atomic(path: &Path, contents: &[u8]) -> anyhow::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path.file_name().and_then(|name| name.to_str()).unwrap_or("poly");
    let tmp = parent.join(format!(".{file_name}.{}.poly.tmp", std::process::id()));
    let original_permissions = fs_err::symlink_metadata(path).ok().map(|meta| meta.permissions());
    fs_err::write(&tmp, contents)?;
    // The rename replaces the original inode with a freshly created temp file,
    // whose mode is `0666 & !umask` and has no relationship to the file being
    // fixed. Without this, fixing an executable script clears its exec bit.
    if let Some(permissions) = original_permissions {
        fs_err::set_permissions(&tmp, permissions)?;
    }
    if let Err(error) = fs_err::rename(&tmp, path) {
        let _ = fs_err::remove_file(&tmp);
        return Err(error.into());
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    use tempfile::TempDir;

    /// The property that motivates the temp-sibling write: a failure part-way
    /// through must leave the author's file exactly as it was. A read-only
    /// parent directory blocks the temp file while leaving the (writable)
    /// destination openable, so it fails at precisely the point a direct write
    /// would already have truncated the destination.
    #[test]
    fn a_failed_write_cannot_truncate_the_destination() {
        let dir = TempDir::new().expect("tempdir");
        let file = dir.path().join("source.rs");
        fs_err::write(&file, "original\n").expect("seed");
        fs_err::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).expect("read-only dir");

        // Running as root (or on a filesystem that ignores the mode) makes the
        // directory writable anyway, and the test would then assert nothing.
        // Probing for that explicitly — rather than treating any success as
        // "must be root" — is what stops this going vacuously green if the
        // atomic write is ever replaced by a direct one.
        if fs_err::write(dir.path().join("probe"), b"x").is_ok() {
            fs_err::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).expect("restore");
            return;
        }

        assert!(
            write_atomic(&file, b"replacement\n").is_err(),
            "a write that cannot create its temp sibling must fail, not fall back to truncating"
        );
        assert_eq!(
            fs_err::read_to_string(&file).expect("read back"),
            "original\n",
            "a write that could not complete must not have touched the destination"
        );

        // …whereas the direct write this replaced destroys the file on the very
        // same failure path.
        fs_err::write(&file, "replacement\n").expect("direct write still permitted");
        assert_eq!(fs_err::read_to_string(&file).expect("read back"), "replacement\n");
        fs_err::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).expect("restore");
    }

    #[test]
    fn write_atomic_replaces_content_and_keeps_the_destination_mode() {
        let dir = TempDir::new().expect("tempdir");
        let script = dir.path().join("hook.sh");
        fs_err::write(&script, "#!/bin/sh\nold\n").expect("seed");
        fs_err::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        write_atomic(&script, b"#!/bin/sh\nnew\n").expect("write");

        assert_eq!(fs_err::read_to_string(&script).expect("read back"), "#!/bin/sh\nnew\n");
        let mode = fs_err::metadata(&script).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "an executable file must not lose its exec bit to a fix");
        assert!(
            std::fs::read_dir(dir.path())
                .expect("read dir")
                .filter_map(Result::ok)
                .all(|entry| entry.file_name() == "hook.sh"),
            "the temp sibling must be renamed away, not left behind"
        );
    }

    /// Defense in depth for the symlink case: even if a caller reached
    /// [`write_atomic`] with a symlink destination, `rename` replaces the link
    /// itself. Nothing is ever written through it.
    #[test]
    fn write_atomic_replaces_a_symlink_instead_of_following_it() {
        let dir = TempDir::new().expect("tempdir");
        let outside = dir.path().join("outside.txt");
        fs_err::write(&outside, "SECRET\n").expect("seed");
        let link = dir.path().join("link.rs");
        std::os::unix::fs::symlink(&outside, &link).expect("symlink");

        write_atomic(&link, b"fix\n").expect("write");

        assert_eq!(
            fs_err::read_to_string(&outside).expect("read target"),
            "SECRET\n",
            "the symlink target must be untouched"
        );
        assert!(
            !fs_err::symlink_metadata(&link)
                .expect("stat link")
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn nothing_that_is_not_a_regular_file_is_read_or_hashed() {
        let dir = TempDir::new().expect("tempdir");
        let target = dir.path().join("target.txt");
        fs_err::write(&target, "content\n").expect("seed");
        let link = dir.path().join("link.txt");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");

        assert!(is_regular_file(&target));
        assert!(!is_regular_file(&link), "a symlink is not a regular file");
        assert!(!is_regular_file(dir.path()), "a directory is not a regular file");
        assert_eq!(hash_file(&link), None, "hashing must not read through the link");
        assert_eq!(read_regular_file(&link), None, "reading must not follow the link");
        assert_eq!(read_regular_file(&target).as_deref(), Some(&b"content\n"[..]));
    }
}
