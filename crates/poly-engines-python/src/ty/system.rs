//! The [`System`] a ty check runs against: the host's loaded file contents laid over the disk.

use std::any::Any;
use std::io::Result;
use std::sync::Arc;

use ruff_db::file_revision::FileRevision;
use ruff_db::system::walk_directory::WalkDirectoryBuilder;
use ruff_db::system::{
    DirectoryEntry, FileType, Metadata, OsSystem, System, SystemPath, SystemPathBuf, SystemVirtualPath, WhichError,
    WhichResult, WritableSystem,
};
use ruff_notebook::{Notebook, NotebookError};
use rustc_hash::FxHashMap;

/// Project files come from memory, everything else (site-packages, extra paths) from the disk.
///
/// The host loads every project file before the call and keys its result cache on those bytes, so
/// reading them again from disk would let a check see content the cache key does not describe.
/// Nothing here runs a process or probes `PATH`: [`System::which`] always misses, and the command
/// executor is the trait default, which refuses.
#[derive(Debug, Clone)]
pub(super) struct OverlaySystem {
    os: OsSystem,
    overlay: Arc<FxHashMap<SystemPathBuf, Arc<str>>>,
}

impl OverlaySystem {
    /// A system rooted at the absolute directory `cwd`, serving `overlay` in place of the disk.
    pub(super) fn new(cwd: &SystemPath, overlay: FxHashMap<SystemPathBuf, Arc<str>>) -> Self {
        Self {
            os: OsSystem::new(cwd),
            overlay: Arc::new(overlay),
        }
    }
}

impl System for OverlaySystem {
    fn path_metadata(&self, path: &SystemPath) -> Result<Metadata> {
        match self.os.path_metadata(path) {
            Err(_) if self.overlay.contains_key(path) => Ok(Metadata::new(FileRevision::new(0), None, FileType::File)),
            other => other,
        }
    }

    fn canonicalize_path(&self, path: &SystemPath) -> Result<SystemPathBuf> {
        self.os.canonicalize_path(path)
    }

    fn is_same_file(&self, first: &SystemPath, second: &SystemPath) -> Result<bool> {
        self.os.is_same_file(first, second)
    }

    fn which(&self, _binary_name: &str) -> WhichResult {
        Err(WhichError::CannotFindBinaryPath)
    }

    fn read_to_string(&self, path: &SystemPath) -> Result<String> {
        match self.overlay.get(path) {
            Some(content) => Ok(content.to_string()),
            None => self.os.read_to_string(path),
        }
    }

    fn read_to_notebook(&self, path: &SystemPath) -> std::result::Result<Notebook, NotebookError> {
        self.os.read_to_notebook(path)
    }

    fn read_virtual_path_to_string(&self, path: &SystemVirtualPath) -> Result<String> {
        self.os.read_virtual_path_to_string(path)
    }

    fn read_virtual_path_to_notebook(&self, path: &SystemVirtualPath) -> std::result::Result<Notebook, NotebookError> {
        self.os.read_virtual_path_to_notebook(path)
    }

    fn current_directory(&self) -> &SystemPath {
        self.os.current_directory()
    }

    fn user_config_directory(&self) -> Option<SystemPathBuf> {
        None
    }

    fn cache_dir(&self) -> Option<SystemPathBuf> {
        None
    }

    fn read_directory<'a>(
        &'a self,
        path: &SystemPath,
    ) -> Result<Box<dyn Iterator<Item = Result<DirectoryEntry>> + 'a>> {
        self.os.read_directory(path)
    }

    fn walk_directory(&self, path: &SystemPath) -> WalkDirectoryBuilder {
        self.os.walk_directory(path)
    }

    fn as_writable(&self) -> Option<&dyn WritableSystem> {
        None
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn dyn_clone(&self) -> Box<dyn System> {
        Box::new(self.clone())
    }
}
