//! Content-hash result cache (blake3). The key folds in the engine id, its
//! version, the resolved engine options, and the file content, so any change to
//! tool, config, or source invalidates the entry. Writes are atomic
//! (sibling-tmp + rename) so concurrent rayon workers never read a torn file.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A content-hash result cache backed by files under the platform cache dir.
pub struct Cache {
    dir: PathBuf,
    enabled: bool,
}

impl Cache {
    /// Open the cache, creating its directory when `enabled`.
    pub fn new(enabled: bool) -> anyhow::Result<Self> {
        let dir = dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("polylint")
            .join("v1");
        if enabled {
            std::fs::create_dir_all(&dir)?;
        }
        Ok(Self { dir, enabled })
    }

    /// Compute the cache key for an engine run over some content.
    pub fn key(engine: &str, version: &str, options: &toml::Table, content: &str) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(engine.as_bytes());
        hasher.update(b"\0");
        hasher.update(version.as_bytes());
        hasher.update(b"\0");
        let opts = toml::to_string(options).unwrap_or_default();
        hasher.update(opts.as_bytes());
        hasher.update(b"\0");
        hasher.update(content.as_bytes());
        hasher.finalize().to_hex().to_string()
    }

    /// Fetch a cached entry by key, or `None` on miss / when disabled.
    pub fn get(&self, key: &str) -> Option<Vec<u8>> {
        if !self.enabled {
            return None;
        }
        std::fs::read(self.dir.join(key)).ok()
    }

    /// Store an entry under `key` (atomic write). No-op when disabled.
    pub fn put(&self, key: &str, bytes: &[u8]) -> anyhow::Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = self
            .dir
            .join(format!(".{key}.{}.{}.tmp", std::process::id(), n));
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, self.dir.join(key))?;
        Ok(())
    }
}
