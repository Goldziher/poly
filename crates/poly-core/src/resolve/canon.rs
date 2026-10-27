//! Memoized path canonicalization for config resolution.
//!
//! A hook run hands the resolver one root per staged file, and each root used to
//! trigger several `realpath` calls. A relative path costs an extra `getcwd` per
//! call on top of that, and `getcwd` is an `open` of every ancestor on macOS, so
//! 1800 roots meant tens of thousands of syscalls before any file was touched.
//! Resolution is asked about the same few directories over and over: anchor
//! relative paths on the working directory once and remember each answer.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

#[derive(Default)]
pub(super) struct Canon {
    cwd: OnceLock<Option<PathBuf>>,
    memo: Mutex<HashMap<PathBuf, Option<PathBuf>>>,
}

impl Canon {
    /// [`std::fs::canonicalize`], memoized, with relative paths anchored on a
    /// working directory read once per [`Canon`].
    pub(super) fn canonicalize(&self, path: &Path) -> Option<PathBuf> {
        let path = if path.as_os_str().is_empty() {
            Path::new(".")
        } else {
            path
        };
        if let Some(hit) = self.memo.lock().ok()?.get(path) {
            return hit.clone();
        }
        let resolved = self.absolute(path).and_then(|abs| std::fs::canonicalize(abs).ok());
        self.memo.lock().ok()?.insert(path.to_path_buf(), resolved.clone());
        resolved
    }

    fn absolute(&self, path: &Path) -> Option<PathBuf> {
        if path.is_absolute() {
            return Some(path.to_path_buf());
        }
        let cwd = self.cwd.get_or_init(|| std::env::current_dir().ok());
        cwd.as_ref().map(|cwd| cwd.join(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_std_canonicalize_for_relative_absolute_and_missing_paths() {
        let canon = Canon::default();
        let cwd = std::env::current_dir().unwrap();

        assert_eq!(canon.canonicalize(Path::new(".")), std::fs::canonicalize(&cwd).ok());
        assert_eq!(canon.canonicalize(Path::new("")), std::fs::canonicalize(&cwd).ok());
        assert_eq!(canon.canonicalize(&cwd), std::fs::canonicalize(&cwd).ok());
        assert_eq!(canon.canonicalize(Path::new("no/such/dir")), None);
    }

    #[test]
    fn answers_repeat_queries_from_the_memo() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let canon = Canon::default();
        let first = canon.canonicalize(&path);
        assert!(first.is_some());
        drop(dir);
        assert_eq!(
            canon.canonicalize(&path),
            first,
            "the directory is gone; only the memo knows it"
        );
    }
}
