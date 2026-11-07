//! The file system a skif run sees: exactly the files the host handed over, and a log of what was asked of it.
//!
//! skif discovers a workspace and reads manifests, lock files, `tsconfig` and sources through a `FileSystem`.
//! Backing it with the host's loaded files (never the disk) makes the run a function of the request, and the
//! project-check cache key already digests every file in the request. The log is how that is checked: every file
//! skif read must be one the request carried, and the paths it probed for and did not find are the inputs whose
//! *appearance* would change the result.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use skif::{DirEntry, FileSystem, MemoryFs};

/// What a run asked of the file system.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReadLog {
    /// Files that were read (or tested for and found).
    pub read: BTreeSet<PathBuf>,
    /// Paths that were looked for and are not in the request.
    pub missed: BTreeSet<PathBuf>,
}

/// An in-memory tree over the request's files that records every lookup.
#[derive(Clone)]
pub(super) struct RecordingFs {
    files: Arc<MemoryFs>,
    log: Arc<Mutex<ReadLog>>,
}

impl RecordingFs {
    /// A tree holding `files` (path, content) and an empty log.
    pub(super) fn new<'a>(files: impl IntoIterator<Item = (PathBuf, &'a str)>) -> Self {
        let mut tree = MemoryFs::new();
        for (path, content) in files {
            tree.insert(path, content);
        }
        Self {
            files: Arc::new(tree),
            log: Arc::default(),
        }
    }

    /// A copy of the log so far.
    pub(super) fn log(&self) -> ReadLog {
        self.log.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn note(&self, path: &Path, found: bool) {
        let mut log = self.log.lock().unwrap_or_else(PoisonError::into_inner);
        let set = if found { &mut log.read } else { &mut log.missed };
        set.insert(path.to_path_buf());
    }
}

impl FileSystem for RecordingFs {
    fn read_to_string(&self, path: &Path) -> io::Result<Arc<str>> {
        let result = self.files.read_to_string(path);
        self.note(path, result.is_ok());
        result
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        self.files.read_dir(path)
    }

    fn is_file(&self, path: &Path) -> bool {
        let found = self.files.is_file(path);
        self.note(path, found);
        found
    }

    fn is_dir(&self, path: &Path) -> bool {
        self.files.is_dir(path)
    }
}
