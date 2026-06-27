//! Content-hash result cache (blake3). The key folds in the engine id, its
//! version, the resolved engine options, and the file content, so any change to
//! tool, config, or source invalidates the entry. Writes are atomic
//! (sibling-tmp + rename) so concurrent rayon workers never read a torn file.
//!
//! The cache lives in a repo-local `.polylint/cache/` directory so it travels
//! with the checkout and is trivially `.gitignore`-able.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Resolve the repo-local cache root by walking up from the current working
/// directory for an anchor, in priority order:
///
/// 1. nearest ancestor containing a `.git` entry (the git top-level) →
///    `<that>/.polylint/cache`,
/// 2. else nearest ancestor containing `polylint.toml` →
///    `<that>/.polylint/cache`,
/// 3. else `./.polylint/cache` relative to the current working directory.
///
/// The `.git` anchor wins over `polylint.toml` even when the latter sits deeper,
/// so the cache is shared across a repository rather than fragmented per
/// sub-package.
fn cache_root() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let anchor = find_anchor(&cwd, ".git").or_else(|| find_anchor(&cwd, "polylint.toml"));
    let base = anchor.unwrap_or(cwd);
    base.join(".polylint").join("cache")
}

/// Return the nearest ancestor of `start` (inclusive) that contains an entry
/// named `marker`, or `None` if no ancestor does.
fn find_anchor(start: &Path, marker: &str) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|dir| dir.join(marker).exists())
        .map(Path::to_path_buf)
}

/// A content-hash result cache backed by files under repo-local `.polylint/cache`.
pub struct Cache {
    dir: PathBuf,
    enabled: bool,
}

impl Cache {
    /// Open the cache, creating its directory when `enabled`.
    pub fn new(enabled: bool) -> anyhow::Result<Self> {
        let dir = cache_root();
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an enabled cache rooted at an explicit (created) directory, so the
    /// test never depends on the process cwd or a real `.git`.
    fn cache_at(dir: PathBuf) -> Cache {
        std::fs::create_dir_all(&dir).unwrap();
        Cache { dir, enabled: true }
    }

    #[test]
    fn get_returns_stored_bytes_on_hit() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = cache_at(tmp.path().to_path_buf());
        let opts = toml::Table::new();
        let key = Cache::key("eng", "1", &opts, "content");
        cache.put(&key, b"stored").unwrap();
        assert_eq!(cache.get(&key).as_deref(), Some(&b"stored"[..]));
    }

    #[test]
    fn miss_when_content_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = cache_at(tmp.path().to_path_buf());
        let opts = toml::Table::new();
        let key = Cache::key("eng", "1", &opts, "content");
        cache.put(&key, b"stored").unwrap();
        let other = Cache::key("eng", "1", &opts, "different content");
        assert_ne!(key, other, "content change must alter the key");
        assert_eq!(cache.get(&other), None);
    }

    #[test]
    fn miss_when_version_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = cache_at(tmp.path().to_path_buf());
        let opts = toml::Table::new();
        let key = Cache::key("eng", "1", &opts, "content");
        cache.put(&key, b"stored").unwrap();
        let other = Cache::key("eng", "2", &opts, "content");
        assert_ne!(key, other, "version change must alter the key");
        assert_eq!(cache.get(&other), None);
    }

    #[test]
    fn miss_when_options_change() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = cache_at(tmp.path().to_path_buf());
        let opts_a = toml::Table::new();
        let mut opts_b = toml::Table::new();
        opts_b.insert("line-length".into(), toml::Value::Integer(120));
        let key = Cache::key("eng", "1", &opts_a, "content");
        cache.put(&key, b"stored").unwrap();
        let other = Cache::key("eng", "1", &opts_b, "content");
        assert_ne!(key, other, "options change must alter the key");
        assert_eq!(cache.get(&other), None);
    }

    #[test]
    fn disabled_cache_is_a_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("nested").join("cache");
        let cache = Cache {
            dir: dir.clone(),
            enabled: false,
        };
        let opts = toml::Table::new();
        let key = Cache::key("eng", "1", &opts, "content");
        cache.put(&key, b"stored").unwrap();
        assert_eq!(cache.get(&key), None, "disabled get must miss");
        assert!(!dir.exists(), "disabled put must not create the cache dir");
    }

    #[test]
    fn find_anchor_prefers_nearest_ancestor_with_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let deep = root.join("a").join("b");
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(find_anchor(&deep, ".git").as_deref(), Some(root));
    }

    #[test]
    fn find_anchor_returns_none_without_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let deep = tmp.path().join("a").join("b");
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(find_anchor(&deep, ".git"), None);
    }

    #[test]
    fn find_anchor_falls_back_to_polylint_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("polylint.toml"), b"").unwrap();
        let deep = root.join("pkg");
        std::fs::create_dir_all(&deep).unwrap();
        // No `.git` anywhere, so `.git` lookup misses and the toml anchor wins.
        assert_eq!(find_anchor(&deep, ".git"), None);
        assert_eq!(find_anchor(&deep, "polylint.toml").as_deref(), Some(root));
    }
}
