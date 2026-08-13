//! Exposing populated submodules in the snapshot as symlinks into the live
//! worktree.
//!
//! Split from the parent module because this is the one part of a refresh that
//! does **not** materialize index content: `git checkout-index` writes no bytes
//! for a gitlink, so a submodule is linked rather than copied (the parent
//! module's docs explain why that is correct). It therefore owns its own
//! filesystem primitives — the idempotent link/replace dance and the
//! platform-specific symlink and removal calls, which have Windows behaviour
//! nothing else in the snapshot needs — and is kept apart from the sanitizing
//! pass so the two symlink stories are never confused: the links created here
//! deliberately point outside the snapshot, and are created *after* the parent's
//! sanitizing pass for exactly that reason.

use std::path::Path;

use tracing::debug;

use super::Error;
use crate::git;

/// Expose each populated submodule in the snapshot as a symlink into the live
/// worktree, so whole-workspace compile hooks can resolve files inside it (see
/// the parent module's docs). An uninitialized submodule (empty worktree
/// directory) is skipped — there is nothing to link and the real build would
/// fail on it too.
pub(super) fn materialize_submodules(root: &Path, dir: &Path) -> Result<(), Error> {
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
