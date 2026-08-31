//! `[hooks] snapshot_include`: named untracked paths linked into the snapshot.
//!
//! The snapshot is byte-faithful to the **index**, which is what makes a commit
//! gate check the bytes a commit would capture. That excludes untracked files —
//! correctly, and it stays the default. But a `workspace` hook whose build reads
//! a gitignored input (a generated file, a local config a type checker loads, a
//! downloaded fixture directory) then fails under the gate while passing in the
//! worktree, and the error names the missing file rather than the isolation that
//! removed it.
//!
//! Entries are **linked, not copied**, for the same reason a populated submodule
//! is: `git checkout-index` writes no bytes for content the index does not hold,
//! so there is nothing to materialize from. Linking also keeps them out of the
//! manifest, which matters more than it looks — [`super::manifest::prune_stale`]
//! deletes any manifest path absent from the current staged set, so a recorded
//! untracked file would be pruned on the very next refresh, while an unrecorded
//! *copy* would go stale forever because pruning only ever touches manifest
//! paths. A link is re-established every refresh and is always current.
//!
//! Like the submodule links, these deliberately point outside the snapshot and
//! are therefore created **after** the parent's symlink-sanitizing pass.

use std::path::Path;

use tracing::{debug, warn};

use super::Error;
use super::submodule::ensure_symlink;
use crate::git;

/// Ledger of the entries currently linked, so an entry the repository withdraws
/// can be unlinked on the next refresh.
///
/// Deliberately **not** the staged manifest: that one is pruned against the
/// index, which holds none of these paths, so recording them there would delete
/// every link on the very next run. Leaving withdrawn links in place is not an
/// option either — a hook would go on reading an untracked file after the
/// repository stopped allowing it.
const INCLUDES_FILE: &str = ".poly-includes";

/// Link each configured entry into `dir` from the live worktree at `root`.
///
/// A missing entry is skipped with a warning rather than failing the run: the
/// whole point is a file git does not track, and one that is simply absent must
/// not turn a lint gate into a hard error.
pub(super) fn materialize_includes(root: &Path, dir: &Path, includes: &[String]) -> Result<(), Error> {
    unlink_withdrawn(dir, includes);
    let mut linked: Vec<&str> = Vec::new();
    for entry in includes {
        let relative = Path::new(entry);
        // Config-time validation already rejects absolute paths and `..`, but
        // this is the point where the value is joined onto a root, so it is
        // checked again here rather than trusted across a crate boundary.
        if !git::is_safe_relative_path(relative) {
            warn!(
                entry,
                "skipping snapshot_include entry that is not a safe relative path"
            );
            continue;
        }
        let source = root.join(relative);
        // `symlink_metadata`, not `exists`: a dangling symlink in the worktree
        // must be reported as such rather than silently treated as missing.
        if std::fs::symlink_metadata(&source).is_err() {
            warn!(entry, "snapshot_include entry does not exist in the worktree");
            continue;
        }
        let target = std::fs::canonicalize(&source).unwrap_or(source);
        ensure_symlink(&target, &dir.join(relative))?;
        linked.push(entry.as_str());
        debug!(entry, "linked snapshot_include entry into the staged snapshot");
    }
    write_ledger(dir, &linked);
    Ok(())
}

/// Remove links this snapshot holds for entries no longer configured.
///
/// Best-effort throughout: a snapshot is a cache, and a ledger that could not be
/// read or a link that could not be removed must not fail a commit gate. The
/// worst case is a stale link that the next successful refresh clears.
fn unlink_withdrawn(dir: &Path, includes: &[String]) {
    let Ok(previous) = std::fs::read_to_string(dir.join(INCLUDES_FILE)) else {
        return;
    };
    for entry in previous.lines().filter(|entry| !entry.is_empty()) {
        if includes.iter().any(|current| current == entry) {
            continue;
        }
        let relative = Path::new(entry);
        if !git::is_safe_relative_path(relative) {
            continue;
        }
        let link = dir.join(relative);
        // Only ever unlink a symlink: if the path is now a real file the
        // checkout materialized, removing it would delete staged content.
        if std::fs::symlink_metadata(&link).is_ok_and(|meta| meta.file_type().is_symlink()) {
            let _ = remove_link(&link);
            debug!(entry, "unlinked a withdrawn snapshot_include entry");
        }
    }
}

#[cfg(unix)]
fn remove_link(link: &Path) -> std::io::Result<()> {
    std::fs::remove_file(link)
}

#[cfg(windows)]
fn remove_link(link: &Path) -> std::io::Result<()> {
    std::fs::remove_dir(link).or_else(|_| std::fs::remove_file(link))
}

/// Record what is linked now. Removed entirely when nothing is, so a repository
/// that never uses the feature leaves no file behind.
fn write_ledger(dir: &Path, linked: &[&str]) {
    let path = dir.join(INCLUDES_FILE);
    if linked.is_empty() {
        let _ = std::fs::remove_file(path);
        return;
    }
    let _ = std::fs::write(path, format!("{}\n", linked.join("\n")));
}
