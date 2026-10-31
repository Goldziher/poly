//! Giving the staged snapshot a `.git` pointer to the original repository.
//!
//! The snapshot materializes the index under a directory outside the repository
//! and exports `GIT_DIR` / `GIT_INDEX_FILE` to every hook, so `git ls-files`
//! and friends work there (see [`crate::runner`]). A tool that reads a
//! repository through a library rather than the `git` binary is not handed that
//! environment, though: ai-rulez resolves a rule glob against the files a
//! repository tracks, finds no repository in the snapshot, and falls back to a
//! filesystem walk that skips hidden directories — so `.agents/plugins/**`
//! matches nothing even though the snapshot holds the file (Goldziher/poly#45).
//!
//! A `.git` **file** containing `gitdir: <absolute git dir>` is what makes the
//! snapshot a work tree to those tools. It adds no capability a hook did not
//! already have — `GIT_DIR` already points at the same repository, and the
//! exported context has always let a hook read (and, deliberately, write) the
//! real index — it only states the same fact in the place a library looks for
//! it. The pointer is a file, not a symlink: a snapshot-local `git init` keeps
//! it as-is instead of following a link, and writing `.git/<x>` through it fails
//! with `ENOTDIR` rather than landing in the real repository.

use std::path::Path;

use tracing::{debug, warn};

use super::submodule::remove_existing_entry;
use crate::git::{self, git_cmd, path_from_git_bytes};

/// The snapshot-relative name of the pointer file.
const POINTER: &str = ".git";

/// Write (or refresh) the `.git` pointer under `dir` for the repository at
/// `root`.
///
/// Best-effort by design: a repository whose git dir cannot be resolved, or a
/// cache directory that refuses the write, leaves the snapshot exactly as it was
/// before this file existed — and no worse than a hook that never asks git
/// anything.
pub(super) fn materialize(root: &Path, dir: &Path) {
    match pointer_for(root) {
        Ok(contents) => write_pointer(dir, &contents),
        Err(error) => warn!(%error, "could not write the snapshot's .git pointer"),
    }
}

/// The pointer's contents, or the git error that prevented resolving them.
fn pointer_for(root: &Path) -> Result<String, git::Error> {
    let output = git_cmd("resolve the repository's git dir")?
        .current_dir(root)
        .arg("rev-parse")
        .arg("--absolute-git-dir")
        .check(true)
        .output()?;
    let git_dir = path_from_git_bytes(output.stdout.trim_ascii())?;
    Ok(format!("gitdir: {}\n", git_dir.display()))
}

/// Replace whatever occupies `.git` with `contents`. A real directory is what a
/// snapshot-local `git init` leaves behind, and it must not survive as a second,
/// empty repository.
fn write_pointer(dir: &Path, contents: &str) {
    let link = dir.join(POINTER);
    if std::fs::read_to_string(&link).is_ok_and(|current| current == contents) {
        return;
    }
    if let Err(error) = remove_existing_entry(&link) {
        warn!(%error, "could not replace the snapshot's .git pointer");
        return;
    }
    match std::fs::write(&link, contents) {
        Ok(()) => debug!(pointer = %link.display(), "wrote the snapshot's .git pointer"),
        Err(error) => warn!(%error, "could not write the snapshot's .git pointer"),
    }
}
