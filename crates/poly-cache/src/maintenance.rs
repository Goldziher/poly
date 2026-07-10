//! Maintenance operations for the [`ResultCache`]: stats, sizing, garbage
//! collection, and a full clean.
//!
//! These operate directly on the on-disk `results/<ns>/` tree and the `VERSION`
//! sentinel, independent of the cache's read/write [`enabled`] flag — pruning a
//! disabled cache is still valid maintenance.
//!
//! [`enabled`]: ResultCache::enabled

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};

use crate::{CACHE_FORMAT_VERSION, Namespace, ResultCache};

/// Per-namespace entry count and on-disk byte total.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceStats {
    /// Which namespace these figures describe.
    pub namespace: Namespace,
    /// Number of cache entries (files) stored in the namespace.
    pub entries: u64,
    /// Total size in bytes of those entries.
    pub bytes: u64,
}

/// A snapshot of the cache's on-disk footprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheStats {
    /// The format version this build expects ([`CACHE_FORMAT_VERSION`]).
    pub format_version: String,
    /// The version recorded in the on-disk `VERSION` sentinel, if present.
    pub on_disk_version: Option<String>,
    /// Per-namespace breakdown, in [`Namespace::ALL`] order.
    pub per_namespace: Vec<NamespaceStats>,
    /// Sum of [`NamespaceStats::bytes`] across every namespace.
    pub total_bytes: u64,
}

/// A single on-disk cache entry, used internally by [`ResultCache::gc`].
struct Entry {
    path: PathBuf,
    size: u64,
    modified: SystemTime,
}

impl ResultCache {
    /// The `results/<ns>/` directory for a namespace.
    fn namespace_dir(&self, namespace: Namespace) -> PathBuf {
        self.root().join("results").join(namespace.as_dir())
    }

    /// Read the on-disk `VERSION` sentinel, trimmed; `None` when it is absent.
    fn read_on_disk_version(&self) -> Option<String> {
        std::fs::read_to_string(self.root().join("VERSION"))
            .ok()
            .map(|version| version.trim().to_string())
    }

    /// Rewrite the `VERSION` sentinel to [`CACHE_FORMAT_VERSION`].
    fn rewrite_version(&self) -> Result<()> {
        let path = self.root().join("VERSION");
        std::fs::write(&path, CACHE_FORMAT_VERSION)
            .with_context(|| format!("failed to write cache VERSION sentinel {}", path.display()))
    }

    /// Collect the entries (real files, skipping `.`-prefixed temporaries) in a
    /// namespace directory. A missing directory yields an empty list.
    fn collect_entries(&self, namespace: Namespace) -> Result<Vec<Entry>> {
        let dir = self.namespace_dir(namespace);
        let read_dir = match std::fs::read_dir(&dir) {
            Ok(read_dir) => read_dir,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(error).with_context(|| format!("failed to read {}", dir.display()));
            }
        };

        let mut entries = Vec::new();
        for item in read_dir {
            let item = item.with_context(|| format!("failed to read entry in {}", dir.display()))?;
            let name = item.file_name();
            if name.to_string_lossy().starts_with('.') {
                continue;
            }
            let metadata = item
                .metadata()
                .with_context(|| format!("failed to stat {}", item.path().display()))?;
            if !metadata.is_file() {
                continue;
            }
            let modified = metadata.modified().unwrap_or_else(|_| SystemTime::now());
            entries.push(Entry {
                path: item.path(),
                size: metadata.len(),
                modified,
            });
        }
        Ok(entries)
    }

    /// Snapshot the cache's on-disk footprint: per-namespace entry counts and
    /// byte totals, plus the format and on-disk versions.
    ///
    /// # Errors
    ///
    /// Returns `Err` if a namespace directory cannot be read or an entry cannot
    /// be stat-ed.
    pub fn stats(&self) -> Result<CacheStats> {
        let mut per_namespace = Vec::with_capacity(Namespace::ALL.len());
        let mut total_bytes = 0u64;
        for namespace in Namespace::ALL {
            let entries = self.collect_entries(namespace)?;
            let bytes: u64 = entries.iter().map(|entry| entry.size).sum();
            total_bytes += bytes;
            per_namespace.push(NamespaceStats {
                namespace,
                entries: entries.len() as u64,
                bytes,
            });
        }
        Ok(CacheStats {
            format_version: CACHE_FORMAT_VERSION.to_string(),
            on_disk_version: self.read_on_disk_version(),
            per_namespace,
            total_bytes,
        })
    }

    /// The total size in bytes of every result entry across all namespaces.
    ///
    /// # Errors
    ///
    /// Returns `Err` if a namespace directory cannot be read.
    pub fn total_size(&self) -> Result<u64> {
        let mut total = 0u64;
        for namespace in Namespace::ALL {
            total += self
                .collect_entries(namespace)?
                .iter()
                .map(|entry| entry.size)
                .sum::<u64>();
        }
        Ok(total)
    }

    /// Delete every result entry across all namespaces, keeping the
    /// `results/<ns>` directories, and rewrite the `VERSION` sentinel.
    ///
    /// Returns the number of bytes freed.
    ///
    /// # Errors
    ///
    /// Returns `Err` if an entry cannot be removed or the sentinel cannot be
    /// rewritten.
    pub fn clean(&self) -> Result<u64> {
        let mut freed = 0u64;
        for namespace in Namespace::ALL {
            for entry in self.collect_entries(namespace)? {
                freed += remove_entry(&entry)?;
            }
        }
        self.rewrite_version()?;
        Ok(freed)
    }

    /// Garbage-collect the cache, returning the number of bytes freed.
    ///
    /// Eviction rules, applied in order:
    ///
    /// 1. If the on-disk `VERSION` sentinel differs from [`CACHE_FORMAT_VERSION`]
    ///    (or is absent), the tree is from an incompatible layout: wipe every
    ///    entry and rewrite the sentinel (equivalent to [`clean`]).
    /// 2. Otherwise, when `max_age` is given, evict entries whose mtime is older
    ///    than `max_age`.
    /// 3. Then, when `max_size` is given and the surviving total still exceeds
    ///    it, evict oldest-first (by mtime) until the total is within budget.
    ///
    /// Deterministic and bounded: each entry is visited once for age eviction
    /// and the survivors are sorted once for size eviction.
    ///
    /// # Errors
    ///
    /// Returns `Err` if an entry cannot be stat-ed or removed, or the sentinel
    /// cannot be rewritten.
    ///
    /// [`clean`]: ResultCache::clean
    pub fn gc(&self, max_age: Option<Duration>, max_size: Option<u64>) -> Result<u64> {
        if self.read_on_disk_version().as_deref() != Some(CACHE_FORMAT_VERSION) {
            return self.clean();
        }

        let mut survivors = Vec::new();
        let mut freed = 0u64;
        let now = SystemTime::now();

        for namespace in Namespace::ALL {
            for entry in self.collect_entries(namespace)? {
                let too_old =
                    max_age.is_some_and(|max_age| now.duration_since(entry.modified).is_ok_and(|age| age > max_age));
                if too_old {
                    freed += remove_entry(&entry)?;
                } else {
                    survivors.push(entry);
                }
            }
        }

        if let Some(max_size) = max_size {
            let mut total: u64 = survivors.iter().map(|entry| entry.size).sum();
            if total > max_size {
                survivors.sort_by_key(|entry| entry.modified);
                for entry in &survivors {
                    if total <= max_size {
                        break;
                    }
                    let removed = remove_entry(entry)?;
                    freed += removed;
                    total = total.saturating_sub(removed);
                }
            }
        }

        Ok(freed)
    }
}

/// Remove a single entry file, returning its size. A `NotFound` (already gone,
/// e.g. a concurrent prune) frees zero bytes rather than erroring.
fn remove_entry(entry: &Entry) -> Result<u64> {
    match std::fs::remove_file(&entry.path) {
        Ok(()) => Ok(entry.size),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error).with_context(|| format!("failed to remove {}", entry.path.display())),
    }
}

#[cfg(test)]
mod tests {
    use std::fs::FileTimes;
    use std::time::{Duration, SystemTime};

    use tempfile::TempDir;

    use crate::{CACHE_FORMAT_VERSION, CacheKey, Namespace, ResultCache};

    fn cache_at(dir: &TempDir) -> ResultCache {
        ResultCache::open(dir.path().join("cache"), true).expect("open cache")
    }

    /// Store `bytes` under a synthetic key in `namespace` and return the key.
    fn put(cache: &ResultCache, namespace: Namespace, name: &str, bytes: &[u8]) -> CacheKey {
        let digest = ResultCache::single_file_digest(name);
        let key = ResultCache::key(namespace, name, "1", &toml::Table::new(), &digest);
        cache.put(namespace, &key, bytes).expect("put");
        key
    }

    /// Backdate an entry's mtime by the given duration.
    fn backdate(cache: &ResultCache, namespace: Namespace, key: &CacheKey, ago: Duration) {
        let path = cache.root().join("results").join(namespace.as_dir()).join(key.as_str());
        let when = SystemTime::now() - ago;
        let file = std::fs::OpenOptions::new().write(true).open(&path).expect("open entry");
        file.set_times(FileTimes::new().set_modified(when)).expect("set mtime");
    }

    #[test]
    fn stats_counts_entries_and_bytes_per_namespace() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = cache_at(&tmp);
        put(&cache, Namespace::Lint, "a", b"12345");
        put(&cache, Namespace::Lint, "b", b"678");
        put(&cache, Namespace::Hook, "c", b"xy");

        let stats = cache.stats().unwrap();
        assert_eq!(stats.format_version, CACHE_FORMAT_VERSION);
        assert_eq!(stats.on_disk_version.as_deref(), Some(CACHE_FORMAT_VERSION));

        let lint = stats
            .per_namespace
            .iter()
            .find(|s| s.namespace == Namespace::Lint)
            .unwrap();
        assert_eq!(lint.entries, 2);
        assert_eq!(lint.bytes, 8);
        let hook = stats
            .per_namespace
            .iter()
            .find(|s| s.namespace == Namespace::Hook)
            .unwrap();
        assert_eq!(hook.entries, 1);
        assert_eq!(hook.bytes, 2);
        assert_eq!(stats.total_bytes, 10);
    }

    #[test]
    fn total_size_matches_sum_of_entry_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = cache_at(&tmp);
        put(&cache, Namespace::Fmt, "a", b"abcd");
        put(&cache, Namespace::Hook, "b", b"ef");
        assert_eq!(cache.total_size().unwrap(), 6);
    }

    #[test]
    fn clean_removes_all_entries_and_reports_freed_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = cache_at(&tmp);
        put(&cache, Namespace::Lint, "a", b"12345");
        put(&cache, Namespace::Hook, "b", b"678");

        let freed = cache.clean().unwrap();
        assert_eq!(freed, 8);
        assert_eq!(cache.total_size().unwrap(), 0);
        let stats = cache.stats().unwrap();
        assert!(stats.per_namespace.iter().all(|s| s.entries == 0));
        assert_eq!(stats.on_disk_version.as_deref(), Some(CACHE_FORMAT_VERSION));
    }

    #[test]
    fn gc_evicts_entries_older_than_max_age_and_keeps_fresh_ones() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = cache_at(&tmp);
        let old = put(&cache, Namespace::Hook, "old", b"stale-bytes");
        put(&cache, Namespace::Hook, "fresh", b"new");
        backdate(&cache, Namespace::Hook, &old, Duration::from_secs(100 * 86_400));

        let freed = cache.gc(Some(Duration::from_secs(86_400)), None).unwrap();
        assert_eq!(freed, "stale-bytes".len() as u64);
        let stats = cache.stats().unwrap();
        let hook = stats
            .per_namespace
            .iter()
            .find(|s| s.namespace == Namespace::Hook)
            .unwrap();
        assert_eq!(hook.entries, 1, "only the fresh entry should remain");
    }

    #[test]
    fn gc_evicts_oldest_first_until_within_max_size() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = cache_at(&tmp);
        let oldest = put(&cache, Namespace::Lint, "oldest", b"AAAA");
        let middle = put(&cache, Namespace::Lint, "middle", b"BBBB");
        put(&cache, Namespace::Lint, "newest", b"CCCC");
        backdate(&cache, Namespace::Lint, &oldest, Duration::from_secs(300));
        backdate(&cache, Namespace::Lint, &middle, Duration::from_secs(150));

        let freed = cache.gc(None, Some(4)).unwrap();
        assert_eq!(freed, 8);
        assert_eq!(cache.total_size().unwrap(), 4);
    }

    #[test]
    fn gc_wipes_everything_when_on_disk_version_is_stale() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = cache_at(&tmp);
        put(&cache, Namespace::Lint, "a", b"12345");
        put(&cache, Namespace::Hook, "b", b"678");
        std::fs::write(cache.root().join("VERSION"), "0").unwrap();

        let freed = cache.gc(None, None).unwrap();
        assert_eq!(freed, 8);
        assert_eq!(cache.total_size().unwrap(), 0);
        let version = std::fs::read_to_string(cache.root().join("VERSION")).unwrap();
        assert_eq!(version, CACHE_FORMAT_VERSION, "sentinel must be rewritten");
    }
}
