//! Mirroring the tracked content of the directories a sparse snapshot does not
//! write, as real directories whose files are hardlinked to the worktree (see
//! [`super::scope`] for why those directories exist at all).
//!
//! A directory *symlink* back into the worktree used to be enough for tools that
//! probe the tree by existence, but it hides every file under it from a walker
//! that does not follow directory symlinks — `ai-rulez validate` resolves its
//! rule globs that way, and a glob over an unchanged directory then matched
//! nothing (`AR101`, Goldziher/poly#45). The mirror therefore has to be a real
//! tree.
//!
//! It is built from the **index entries** under each link root, not from a walk
//! of the worktree: untracked content (a `target/`, a `node_modules/`) is not
//! part of the snapshot's contract and must not be pulled in, and the tracked
//! paths are already in hand. Each tracked file is
//! [`std::fs::hard_link`]ed from the worktree, so unchanged content costs one
//! inode reference rather than a byte copy and keeps its worktree mtime. A file
//! the worktree cannot supply is materialized from its index blob by the
//! refresh's checkout. When hardlinking is impossible (a cross-device cache
//! directory, an unsupported filesystem) the bytes are copied instead, with the
//! source mtime preserved. Tracked symlinks are recreated as symlinks; their
//! target stays repository content, exactly as it was.
//!
//! A mirror lives entirely inside the snapshot: unlinking a hardlink in it never
//! touches the worktree file it shares an inode with, and a hook that *writes*
//! through one changes the worktree inode — which [`super::LinkGuard`] detects
//! and fails the run for, the same way it did when the whole directory was a
//! symlink.
//!
//! `.poly-links` records the mirrored roots so a refresh can dismantle the ones
//! the new selection no longer links *before* checking out: a path that moved
//! from mirrored to selected must be written into a real directory, never left
//! behind as a hardlink into a possibly-dirty worktree. A root the selection
//! keeps is reconciled in place — an entry already hardlinked to the current
//! worktree inode is left alone, so a steady-state refresh does not churn inodes
//! or directory mtimes.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use tracing::debug;

use super::Error;
use super::submodule::remove_existing_entry;
use crate::git::{self, StagedEntry};

const LINKS_FILE: &str = ".poly-links";

/// The mirror roots the last refresh recorded under `dir`, as repo-relative
/// directories.
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

/// Remove the mirrors the previous refresh created that `wanted` does not keep.
///
/// Only paths the ledger records are touched. Removing a mirror unlinks the
/// snapshot's hardlinks; the worktree files they point at keep their own link
/// and their bytes.
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
        let path = dir.join(entry);
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            // A snapshot written by an older poly linked directories instead.
            let _ = std::fs::remove_file(&path);
        } else if meta.is_dir() {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

/// Mirror each of `links` under `dir` from the same path under `root`, record
/// them, and return the entries the worktree could not supply so the caller can
/// materialize them from their index blobs.
pub(super) fn materialize(
    root: &Path,
    dir: &Path,
    links: &[PathBuf],
    staged: &[StagedEntry],
) -> Result<Vec<PathBuf>, Error> {
    let roots: Vec<&Path> = links
        .iter()
        .map(PathBuf::as_path)
        .filter(|link| git::is_safe_relative_path(link))
        .collect();
    if roots.is_empty() {
        let _ = std::fs::remove_file(dir.join(LINKS_FILE));
        return Ok(Vec::new());
    }
    let root = std::fs::canonicalize(root)?;
    let wanted: HashSet<&Path> = roots.iter().copied().collect();

    // Prepare first, so an entry whose worktree copy is missing has a real
    // directory to be checked out into rather than a half-written path.
    let mut prepared: Vec<&Path> = Vec::with_capacity(roots.len());
    let mut expected: HashMap<&Path, HashSet<&Path>> = HashMap::new();
    for &link in &roots {
        prepare(&dir.join(link))?;
        prepared.push(link);
        expected.insert(link, HashSet::new());
    }

    let mut from_index: Vec<PathBuf> = Vec::new();
    for entry in staged {
        let Some(link) = entry
            .path
            .ancestors()
            .skip(1)
            .find(|ancestor| wanted.contains(*ancestor))
        else {
            continue;
        };
        let Some(paths) = expected.get_mut(link) else {
            continue;
        };
        let rel = entry.path.strip_prefix(link).unwrap_or(&entry.path);
        paths.insert(rel);
        materialize_entry(&root, &dir.join(link), rel, entry, &mut from_index)?;
    }

    for (&link, paths) in &expected {
        prune_mirror(&dir.join(link), Path::new(""), paths)?;
    }

    if prepared.is_empty() {
        let _ = std::fs::remove_file(dir.join(LINKS_FILE));
    } else {
        let made: Vec<String> = prepared.iter().map(|link| link.display().to_string()).collect();
        std::fs::write(dir.join(LINKS_FILE), format!("{}\n", made.join("\n")))?;
    }
    Ok(from_index)
}

/// Turn `dest` into a directory, replacing a directory symlink an older poly
/// left there or any other non-directory entry.
fn prepare(dest: &Path) -> Result<(), Error> {
    if std::fs::symlink_metadata(dest).is_ok_and(|meta| !meta.is_dir()) {
        remove_existing_entry(dest)?;
    }
    std::fs::create_dir_all(dest)?;
    Ok(())
}

/// Mirror one tracked entry. A missing or wrong-typed worktree entry is pushed
/// to `from_index`; the caller checks it out from the index.
fn materialize_entry(
    root: &Path,
    dest_root: &Path,
    rel: &Path,
    entry: &StagedEntry,
    from_index: &mut Vec<PathBuf>,
) -> Result<(), Error> {
    let source = root.join(&entry.path);
    let dest = dest_root.join(rel);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let Ok(meta) = std::fs::symlink_metadata(&source) else {
        from_index.push(entry.path.clone());
        return Ok(());
    };

    if entry.is_symlink {
        if !meta.file_type().is_symlink() {
            from_index.push(entry.path.clone());
            return Ok(());
        }
        let target = std::fs::read_link(&source)?;
        if std::fs::read_link(&dest).is_ok_and(|current| current == target) {
            return Ok(());
        }
        remove_existing_entry(&dest)?;
        create_symlink(&target, &source, &dest)?;
        return Ok(());
    }

    if !meta.file_type().is_file() {
        // A submodule (or anything else git tracks without a file here) has no
        // worktree file to mirror; its index entry is the only source.
        from_index.push(entry.path.clone());
        return Ok(());
    }

    if same_file(&source, &dest) {
        return Ok(());
    }
    remove_existing_entry(&dest)?;
    match std::fs::hard_link(&source, &dest) {
        Ok(()) => Ok(()),
        Err(error) => {
            debug!(
                path = %entry.path.display(),
                %error,
                "hardlinking into the snapshot failed; copying the worktree file instead"
            );
            copy_preserving_mtime(&source, &dest)
        }
    }
}

/// Whether `dest` is already the same inode as `source` — a hardlink the last
/// refresh made, which neither needs replacing nor should be recreated (that
/// would churn directory mtimes and break a tool's incremental cache).
#[cfg(unix)]
fn same_file(source: &Path, dest: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;

    match (std::fs::metadata(source), std::fs::symlink_metadata(dest)) {
        (Ok(source), Ok(dest)) => source.dev() == dest.dev() && source.ino() == dest.ino(),
        _ => false,
    }
}

/// Portable fallback: hardlinks share size and mtime with their source, and a
/// copied fallback is stamped with the source's mtime, so equality is the same
/// evidence without inode numbers.
#[cfg(not(unix))]
fn same_file(source: &Path, dest: &Path) -> bool {
    match (std::fs::metadata(source), std::fs::metadata(dest)) {
        (Ok(source), Ok(dest)) => {
            source.len() == dest.len()
                && source
                    .modified()
                    .ok()
                    .zip(dest.modified().ok())
                    .is_some_and(|(a, b)| a == b)
        }
        _ => false,
    }
}

/// Copy `source` to `dest` and give it `source`'s mtime, so a fallback copy is
/// indistinguishable from a hardlink to a compiler's staleness check.
fn copy_preserving_mtime(source: &Path, dest: &Path) -> Result<(), Error> {
    std::fs::copy(source, dest)?;
    if let Ok(mtime) = std::fs::metadata(source).and_then(|meta| meta.modified())
        && let Ok(file) = std::fs::File::options().write(true).open(dest)
    {
        let _ = file.set_modified(mtime);
    }
    Ok(())
}

/// Create a symlink at `dest` with the same (repository-relative) target as the
/// worktree's `source`. Windows needs to know whether the target is a directory.
#[cfg(unix)]
fn create_symlink(target: &Path, _source: &Path, dest: &Path) -> Result<(), Error> {
    std::os::unix::fs::symlink(target, dest)?;
    Ok(())
}

#[cfg(windows)]
fn create_symlink(target: &Path, source: &Path, dest: &Path) -> Result<(), Error> {
    if source.is_dir() {
        std::os::windows::fs::symlink_dir(target, dest)?;
    } else {
        std::os::windows::fs::symlink_file(target, dest)?;
    }
    Ok(())
}

/// Remove everything under `dir` that the current index no longer tracks, and
/// report whether `dir` is left empty. A mirror is entirely ours, so an extra
/// entry is a stale path (from an earlier index) or a hook's untracked side
/// effect — both discarded, as the staged snapshot discards untracked writes.
fn prune_mirror(dir: &Path, prefix: &Path, expected: &HashSet<&Path>) -> std::io::Result<bool> {
    let mut empty = true;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let rel = prefix.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            if prune_mirror(&entry.path(), &rel, expected)? {
                let _ = std::fs::remove_dir(entry.path());
            } else {
                empty = false;
            }
        } else if expected.contains(rel.as_path()) {
            empty = false;
        } else {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    Ok(empty)
}
