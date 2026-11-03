//! The exclusive file lock guarding a shared hook source cache directory. The
//! pinned revisions themselves live in `poly.lock` ([`crate::polylock`]).

use std::fs::{File, OpenOptions};
use std::path::Path;

use anyhow::Context;
use fs2::FileExt;

pub(super) struct SourceLock(File);

impl Drop for SourceLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

pub(super) fn lock_source(source_cache: &Path) -> anyhow::Result<SourceLock> {
    std::fs::create_dir_all(source_cache)
        .with_context(|| format!("creating hook source cache {}", source_cache.display()))?;
    let path = source_cache.join("source.lock");
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("opening hook source lock {}", path.display()))?;
    file.lock_exclusive()
        .with_context(|| format!("locking hook source {}", path.display()))?;
    Ok(SourceLock(file))
}
