//! Linking the directories a sparse snapshot does not write to their worktree
//! counterparts (see [`super::scope`] for why).
//!
//! The links point outside the snapshot on purpose, like the submodule and
//! `snapshot_include` links, so they are created after the symlink-sanitizing
//! pass and kept out of the manifest. A ledger records them so a refresh can
//! remove each one the new selection no longer links *before* checking out: an
//! entry that moved from linked to selected must be written into a real
//! directory, never through a stale link into the live worktree. A link the
//! selection keeps is left alone, so a steady-state refresh costs one `lstat`
//! per link.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use tracing::debug;

use super::Error;
use super::submodule::ensure_symlink;
use crate::git;

const LINKS_FILE: &str = ".poly-links";

/// The links the last refresh recorded under `dir`, as repo-relative directories.
pub(super) fn recorded(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_to_string(dir.join(LINKS_FILE))
        .map(|text| {
            text.lines()
                .map(PathBuf::from)
                .filter(|entry| git::is_safe_relative_path(entry))
                .collect()
        })
        .unwrap_or_default()
}

/// Remove the links the previous refresh created that `wanted` does not keep.
/// Only symlinks are removed: the ledger is a cache, and a path that is a real
/// directory now is not ours.
pub(super) fn drop_stale(dir: &Path, wanted: &[PathBuf]) {
    let Ok(previous) = std::fs::read_to_string(dir.join(LINKS_FILE)) else {
        return;
    };
    let wanted: HashSet<&Path> = wanted.iter().map(PathBuf::as_path).collect();
    for entry in previous
        .lines()
        .map(Path::new)
        .filter(|entry| git::is_safe_relative_path(entry) && !wanted.contains(entry))
    {
        let link = dir.join(entry);
        if is_symlink(&link) {
            let _ = std::fs::remove_file(&link);
        }
    }
}

fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
}

/// Link each of `links` under `dir` to the same path under `root`, skipping any
/// the worktree does not have, and record them.
pub(super) fn create(root: &Path, dir: &Path, links: &[PathBuf]) -> Result<(), Error> {
    let root = std::fs::canonicalize(root)?;
    let mut made: Vec<String> = Vec::with_capacity(links.len());
    for link in links.iter().filter(|link| git::is_safe_relative_path(link)) {
        if !is_symlink(&dir.join(link)) {
            let target = root.join(link);
            if !target.is_dir() {
                debug!(path = %link.display(), "not linking a directory the worktree does not have");
                continue;
            }
            ensure_symlink(&target, &dir.join(link))?;
        }
        made.push(link.display().to_string());
    }
    if made.is_empty() {
        let _ = std::fs::remove_file(dir.join(LINKS_FILE));
    } else {
        std::fs::write(dir.join(LINKS_FILE), format!("{}\n", made.join("\n")))?;
    }
    Ok(())
}
