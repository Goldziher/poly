//! `poly-hooks.lock` — the pinned revision per Git hook source, plus the
//! exclusive file lock guarding a shared source cache directory.

use std::fs::{File, OpenOptions};
use std::path::Path;

use anyhow::{Context, bail};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

pub(super) const LOCK_FILE_NAME: &str = "poly-hooks.lock";

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct HookSourceLock {
    pub(super) version: u32,
    pub(super) sources: Vec<LockedSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct LockedSource {
    pub(super) id: String,
    pub(super) source: String,
    pub(super) revision: String,
    pub(super) path: String,
}

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

pub(super) fn write_lock(root: &Path, lock: &HookSourceLock) -> anyhow::Result<()> {
    let path = root.join(LOCK_FILE_NAME);
    let temporary = root.join(format!("{LOCK_FILE_NAME}.tmp"));
    std::fs::write(
        &temporary,
        toml::to_string_pretty(lock).context("serializing hook source lock")?,
    )
    .with_context(|| format!("writing {}", temporary.display()))?;
    std::fs::rename(&temporary, &path).with_context(|| format!("installing {}", path.display()))
}
pub(super) fn remove_lock(root: &Path) -> anyhow::Result<()> {
    let path = root.join(LOCK_FILE_NAME);
    if path.is_file() {
        std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    }
    Ok(())
}
pub(super) fn read_lock(root: &Path) -> anyhow::Result<Option<HookSourceLock>> {
    let path = root.join(LOCK_FILE_NAME);
    if !path.is_file() {
        return Ok(None);
    }
    let lock: HookSourceLock =
        toml::from_str(&std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?)
            .with_context(|| format!("parsing {}", path.display()))?;
    if lock.version != 1 {
        bail!("unsupported {} version {}; expected 1", LOCK_FILE_NAME, lock.version);
    }
    Ok(Some(lock))
}
