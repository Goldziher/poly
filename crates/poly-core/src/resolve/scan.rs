//! The single tree walk behind both nested-config discovery and file discovery.
//!
//! Finding nested `poly.toml` files needs a full walk, and discovery needs the
//! same walk again with the effective excludes applied, which are only known
//! once the configs are. Walking twice made warm runs filesystem-bound. So this
//! walk records every file and directory it sees (with the built-in prune set
//! and the ignore files already applied) and discovery later replays the record
//! in memory, applying the excludes and pruning excluded subtrees logically.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use ignore::{WalkBuilder, WalkState};

use crate::discover::{PrunePolicy, keep_walk_entry};

/// One file or directory the scan walk saw below a walk root.
#[derive(Debug)]
pub(crate) struct SnapshotEntry {
    pub(crate) path: PathBuf,
    pub(crate) is_dir: bool,
}

/// Everything the scan walk saw below one walk root.
#[derive(Debug, Default)]
pub(crate) struct RootSnapshot {
    /// Files and directories kept, sorted by path so that a directory is
    /// immediately followed by everything beneath it.
    pub(crate) entries: Vec<SnapshotEntry>,
    /// Directories the built-in prune set dropped, so the replay can still
    /// report them.
    pub(crate) pruned: Vec<PathBuf>,
}

/// What the scan walk saw, per walk root.
#[derive(Debug, Default)]
pub(super) struct WalkSnapshot(Mutex<HashMap<PathBuf, RootSnapshot>>);

impl WalkSnapshot {
    /// Hand over the recorded entries of `root`, once. A second request, or one
    /// for a root that was never scanned, gets `None` and the caller walks live.
    pub(super) fn take(&self, root: &Path) -> Option<RootSnapshot> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).remove(root)
    }
}

/// Per-thread buffer, flushed into the shared sink when the worker finishes so
/// the hot path takes no lock.
struct Collector<'a> {
    config_dirs: Vec<PathBuf>,
    entries: Vec<SnapshotEntry>,
    sink: &'a Mutex<(Vec<PathBuf>, Vec<SnapshotEntry>)>,
}

impl Drop for Collector<'_> {
    fn drop(&mut self) {
        let mut sink = self.sink.lock().unwrap_or_else(PoisonError::into_inner);
        sink.0.append(&mut self.config_dirs);
        sink.1.append(&mut self.entries);
    }
}

/// Scan `roots` for every directory containing a config file, respecting
/// `.gitignore` and the same pruned-directory set as discovery — including the
/// run's `[discovery] no_prune` opt-out, so the two walks agree on what exists.
/// Every file and directory seen is also recorded for discovery to replay.
///
/// A `poly.toml` inside a still-pruned directory remains undiscoverable, which
/// is why `no_prune` is read from the run's root config rather than from a
/// config nested inside the directory it would un-prune.
pub(super) fn scan_config_dirs(roots: &[PathBuf], no_prune: &[String]) -> (Vec<PathBuf>, WalkSnapshot) {
    let mut config_dirs = Vec::new();
    let mut snapshot = HashMap::new();
    for root in roots.iter().filter(|root| !root.is_file()) {
        let pruned = Arc::new(Mutex::new(Vec::new()));
        let mut builder = WalkBuilder::new(root);
        builder
            .hidden(false)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .parents(true)
            .filter_entry({
                // `filter_entry` demands a `'static` closure, and this walk runs
                // once per root, so the policy owns its copy of the opt-out list.
                let policy = PrunePolicy::new(root, no_prune);
                let pruned = Arc::clone(&pruned);
                move |entry| {
                    let keep = keep_walk_entry(entry, &policy);
                    if !keep {
                        pruned
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .push(entry.path().to_path_buf());
                    }
                    keep
                }
            });
        let sink = Mutex::new((Vec::new(), Vec::new()));
        // The tree is walked on every worker thread: this scan reads directories
        // and nothing else, so it is bound by the filesystem, not by the CPU.
        builder.build_parallel().run(|| {
            let mut collector = Collector {
                config_dirs: Vec::new(),
                entries: Vec::new(),
                sink: &sink,
            };
            Box::new(move |entry| {
                let Ok(entry) = entry else {
                    return WalkState::Continue;
                };
                if entry.depth() == 0 {
                    return WalkState::Continue;
                }
                let Some(file_type) = entry.file_type() else {
                    return WalkState::Continue;
                };
                let is_dir = file_type.is_dir();
                if !is_dir && !file_type.is_file() {
                    return WalkState::Continue;
                }
                let is_config = !is_dir
                    && entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| poly_config::CONFIG_FILE_NAMES.contains(&name));
                if is_config && let Some(dir) = entry.path().parent() {
                    collector.config_dirs.push(dir.to_path_buf());
                }
                collector.entries.push(SnapshotEntry {
                    path: entry.into_path(),
                    is_dir,
                });
                WalkState::Continue
            })
        });
        let (dirs, mut entries) = sink.into_inner().unwrap_or_else(PoisonError::into_inner);
        config_dirs.extend(dirs);
        // Workers finish in no particular order; a fixed order keeps the replay
        // (and so the discovered file list) deterministic.
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        let mut pruned = std::mem::take(&mut *pruned.lock().unwrap_or_else(PoisonError::into_inner));
        pruned.sort();
        snapshot.insert(root.clone(), RootSnapshot { entries, pruned });
    }
    // A config's id is its position here, so the order is fixed by path rather
    // than left to the scheduler.
    config_dirs.sort();
    config_dirs.dedup();
    (config_dirs, WalkSnapshot(Mutex::new(snapshot)))
}
