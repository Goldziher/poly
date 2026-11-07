//! What a run was asked to look at, for an engine whose project check reads a directory tree of its
//! own (see [`Engine::project_inputs`](crate::engine::Engine::project_inputs)).
//!
//! Such an engine judges the whole tree whatever paths the run named, because a verdict over one
//! file of a log is not the verdict over the log. The run's paths then decide only which of its
//! findings are reported, so `poly lint docs/adr/0003-x.md` reports on that record and not on its
//! siblings, and a run that names nothing near the tree does not read it at all.

use std::path::{Path, PathBuf};

use rustc_hash::{FxHashMap, FxHashSet};

use crate::discover::DiscoveredFile;
use crate::engine::Diagnostic;

/// The paths a run named: directories (everything beneath them is in scope) and files.
pub(super) struct RunScope {
    dirs: Vec<PathBuf>,
    /// Canonical path to the path as the run spelled it, which is how the per-file results key it.
    files: FxHashMap<PathBuf, PathBuf>,
}

/// `path` with symlinks resolved. A path that does not exist (a record the lock names but the log
/// lost) resolves through its parent, so it still compares equal to the same path spelled another way.
pub(super) fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path)
        .or_else(|error| match (path.parent(), path.file_name()) {
            (Some(parent), Some(name)) => std::fs::canonicalize(parent).map(|parent| parent.join(name)),
            _ => Err(error),
        })
        .unwrap_or_else(|_| path.to_path_buf())
}

/// The canonical paths among `files` that `reported` names: what discovery kept, out of the files
/// a whole-tree check was handed. Only files sharing a name with a finding are resolved, since
/// canonicalizing every discovered file to answer for a handful of findings would cost a syscall each.
pub(super) fn covered(files: &[DiscoveredFile], reported: &[(PathBuf, Vec<Diagnostic>)]) -> FxHashSet<PathBuf> {
    let names: FxHashSet<&std::ffi::OsStr> = reported.iter().filter_map(|(path, _)| path.file_name()).collect();
    files
        .iter()
        .filter(|file| file.path.file_name().is_some_and(|name| names.contains(name)))
        .map(|file| canonical(&file.path))
        .collect()
}

impl RunScope {
    pub(super) fn new(paths: &[PathBuf]) -> Self {
        let mut scope = Self {
            dirs: Vec::new(),
            files: FxHashMap::default(),
        };
        for path in paths {
            let resolved = canonical(path);
            if path.is_dir() {
                scope.dirs.push(resolved);
            } else {
                scope.files.insert(resolved, path.clone());
            }
        }
        scope
    }

    /// Whether anything the run named is, or contains, something under `dir`.
    pub(super) fn reaches(&self, dir: &Path) -> bool {
        let dir = canonical(dir);
        self.dirs
            .iter()
            .any(|named| named.starts_with(&dir) || dir.starts_with(named))
            || self.files.keys().any(|file| file.starts_with(&dir))
    }

    /// The path to report `finding` under, or `None` when the run did not name it or a directory
    /// above it.
    pub(super) fn admit(&self, finding: &Path) -> Option<PathBuf> {
        let resolved = canonical(finding);
        if let Some(spelled) = self.files.get(&resolved) {
            return Some(spelled.clone());
        }
        self.dirs
            .iter()
            .any(|named| resolved.starts_with(named))
            .then(|| finding.to_path_buf())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_named_file_is_admitted_under_the_spelling_the_run_used() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("log/0001.md");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "x").unwrap();
        let sibling = dir.path().join("log/0002.md");
        std::fs::write(&sibling, "x").unwrap();
        let roundabout = dir.path().join("log/../log/0001.md");
        let scope = RunScope::new(std::slice::from_ref(&roundabout));
        assert_eq!(scope.admit(&file), Some(roundabout));
        assert_eq!(scope.admit(&sibling), None);
    }

    #[test]
    fn a_named_directory_admits_everything_beneath_it_even_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("log")).unwrap();
        let scope = RunScope::new(&[dir.path().join("log")]);
        let gone = dir.path().join("log/0009-gone.md");
        assert_eq!(scope.admit(&gone), Some(gone.clone()));
        assert_eq!(scope.admit(&dir.path().join("elsewhere/x.md")), None);
    }

    #[test]
    fn reaches_means_the_run_is_in_the_directory_or_above_it() {
        let dir = tempfile::tempdir().unwrap();
        for sub in ["log", "other"] {
            std::fs::create_dir_all(dir.path().join(sub)).unwrap();
        }
        let log = dir.path().join("log");
        assert!(RunScope::new(&[dir.path().to_path_buf()]).reaches(&log));
        assert!(RunScope::new(std::slice::from_ref(&log)).reaches(&log));
        assert!(!RunScope::new(&[dir.path().join("other")]).reaches(&log));
        std::fs::write(log.join("a.md"), "x").unwrap();
        assert!(RunScope::new(&[log.join("a.md")]).reaches(&log));
        assert!(!RunScope::new(&[]).reaches(&log));
    }
}
