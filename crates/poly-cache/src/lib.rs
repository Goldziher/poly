//! Unified result-cache for polylint, polyfmt, and poly hooks.
//!
//! # Overview
//!
//! [`ResultCache`] is a blake3 content-hash result cache backed by files under a
//! repo-local `.polylint/cache/` directory. It generalises the single-file key that
//! `polylint-core` uses for lint and format results into a **file-set digest**
//! ([`InputDigest`]), enabling multi-file caching for hook results without changing
//! the single-file path.
//!
//! # Storage layout
//!
//! ```text
//! <repo-root>/.polylint/cache/
//!   VERSION              — format-version sentinel; bump CACHE_FORMAT_VERSION on
//!                          breaking layout changes so GC can detect stale trees
//!   .lock                — advisory lock PLACEHOLDER for GC/clean ops; routine
//!                          get/put use atomic sibling-tmp + rename instead (see
//!                          ADVISORY_LOCK_NOTE below)
//!   results/
//!     lint/<hex-key>     — serde_json-encoded Vec<Diagnostic>
//!     fmt/<hex-key>      — UTF-8 formatted text
//!     hook/<hex-key>     — JSON-encoded hook outcome
//! ```
//!
//! # Key derivation
//!
//! ```text
//! input_digest  = blake3( concat(path \0 blake3(file_bytes)_raw  for each file, sorted by path) )
//!
//! cache_key     = blake3( namespace_dir \0 id \0 version \0 toml(args) \0 input_digest_hex )
//! ```
//!
//! For a single file use [`ResultCache::single_file_digest`]; for a matched hook file
//! set use [`ResultCache::file_set_digest`].
//!
//! # Adoption path for `polylint-core/src/runner.rs`
//!
//! The migration is a near one-line swap per call site.
//!
//! **Before** (using the private `polylint_core::cache::Cache`):
//!
//! ```rust,ignore
//! use crate::cache::Cache;
//!
//! // lint
//! let key = Cache::key(&format!("lint:{}", engine.name()), engine.version(), &ecfg.options, &src.content);
//! cache.get(&key)
//! cache.put(&key, &bytes)
//!
//! // fmt
//! let key = Cache::key(&format!("fmt:{}", engine.name()), engine.version(), &ecfg.options, &current);
//! cache.get(&key)
//! cache.put(&key, out.as_bytes())
//! ```
//!
//! **After** (using `poly_cache`):
//!
//! ```rust,ignore
//! use poly_cache::{Namespace, ResultCache};
//!
//! // lint
//! let digest = ResultCache::single_file_digest(&src.content);
//! let key = ResultCache::key(Namespace::Lint, engine.name(), engine.version(), &ecfg.options, &digest);
//! cache.get(Namespace::Lint, &key)
//! cache.put(Namespace::Lint, &key, &bytes)
//!
//! // fmt
//! let digest = ResultCache::single_file_digest(&current);
//! let key = ResultCache::key(Namespace::Fmt, engine.name(), engine.version(), &ecfg.options, &digest);
//! cache.get(Namespace::Fmt, &key)
//! cache.put(Namespace::Fmt, &key, out.as_bytes())
//! ```
//!
//! # Advisory lock note
//!
//! `get`/`put` operations do **not** acquire a lock — they rely on atomic rename.
//! The `.lock` placeholder exists for future maintenance commands (`poly cache gc`,
//! `poly cache clean`) that need exclusive access to the whole tree.  When those
//! are implemented, add `fd-lock` or `fs2` to the workspace and open `.lock` with
//! an exclusive `FileLock` before mutating the directory tree.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// On-disk format version written to the `VERSION` sentinel file.
///
/// Increment this whenever the cache layout changes incompatibly.  Tools such
/// as `poly cache gc` compare the sentinel against this value to decide whether
/// an existing tree is safe to reuse.
pub const CACHE_FORMAT_VERSION: &str = "2";

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------
// Anchor walk
// ---------------------------------------------------------------------------

/// Return the nearest ancestor of `start` (inclusive) that contains a
/// filesystem entry named `marker`, or `None` if no ancestor does.
///
/// Used by [`root_from`] to locate the repository root.
pub fn find_anchor(start: &Path, marker: &str) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|dir| dir.join(marker).exists())
        .map(Path::to_path_buf)
}

/// Resolve the repo-local cache root directory from `start`, walking upward in
/// priority order:
///
/// 1. nearest ancestor that contains a `.git` entry → `<that>/.polylint/cache`
/// 2. else nearest ancestor that contains `poly.toml` → `<that>/.polylint/cache`
/// 3. else nearest ancestor that contains `polylint.toml` → `<that>/.polylint/cache`
/// 4. else `<start>/.polylint/cache`
///
/// The `.git` anchor wins even when a config file sits deeper, so the cache is
/// shared across a repository rather than fragmented per sub-package.
pub fn root_from(start: &Path) -> PathBuf {
    let anchor = find_anchor(start, ".git")
        .or_else(|| find_anchor(start, "poly.toml"))
        .or_else(|| find_anchor(start, "polylint.toml"));
    let base = anchor.unwrap_or_else(|| start.to_path_buf());
    base.join(".polylint").join("cache")
}

/// Resolve the repo-local cache root from the current working directory.
///
/// Equivalent to `root_from(&std::env::current_dir()?)`.
pub fn root_from_cwd() -> anyhow::Result<PathBuf> {
    let cwd = std::env::current_dir()
        .map_err(|e| anyhow::anyhow!("could not read current directory: {e}"))?;
    Ok(root_from(&cwd))
}

// ---------------------------------------------------------------------------
// Namespace
// ---------------------------------------------------------------------------

/// Cache namespace — routes entries into distinct sub-directories under
/// `results/`.
///
/// | Variant | Sub-directory   | Value format                     |
/// |---------|-----------------|----------------------------------|
/// | `Lint`  | `results/lint/` | `serde_json`-encoded diagnostics |
/// | `Fmt`   | `results/fmt/`  | UTF-8 formatted text             |
/// | `Hook`  | `results/hook/` | JSON-encoded hook outcome        |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Namespace {
    /// Lint-diagnostic results (`Vec<Diagnostic>` JSON bytes).
    Lint,
    /// Formatter output (UTF-8 bytes).
    Fmt,
    /// Hook execution result (opaque JSON bytes).
    Hook,
}

impl Namespace {
    /// The sub-directory component used in the storage path.
    pub fn as_dir(self) -> &'static str {
        match self {
            Namespace::Lint => "lint",
            Namespace::Fmt => "fmt",
            Namespace::Hook => "hook",
        }
    }
}

// ---------------------------------------------------------------------------
// InputDigest
// ---------------------------------------------------------------------------

/// A blake3 digest over one or more input files, used as the content component
/// of a [`CacheKey`].
///
/// Construct with:
/// - [`ResultCache::single_file_digest`] — for a single file (lint / fmt).
/// - [`ResultCache::file_set_digest`] — for a set of matched files (hooks).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputDigest(String);

impl InputDigest {
    /// The hex-encoded digest string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for InputDigest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ---------------------------------------------------------------------------
// CacheKey
// ---------------------------------------------------------------------------

/// An opaque hex-encoded cache key produced by [`ResultCache::key`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheKey(String);

impl CacheKey {
    /// The raw hex string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CacheKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ---------------------------------------------------------------------------
// ResultCache
// ---------------------------------------------------------------------------

/// A content-hash result cache backed by files under `<repo>/.polylint/cache/`.
///
/// `ResultCache` is `Send + Sync`: individual puts are atomic (sibling-tmp +
/// rename) so concurrent rayon workers never read a torn file.
///
/// # Disabled mode
///
/// When constructed with `enabled = false`, every `get` returns `None` and
/// every `put` is a no-op.  The directory is not created.
#[derive(Debug)]
pub struct ResultCache {
    /// `<repo>/.polylint/cache/`
    root: PathBuf,
    enabled: bool,
}

impl ResultCache {
    // -----------------------------------------------------------------------
    // Constructors
    // -----------------------------------------------------------------------

    /// Open the cache at an explicit `root` directory.
    ///
    /// When `enabled`, creates the full sub-directory tree and writes the
    /// `VERSION` sentinel.  When disabled, returns a no-op stub.
    pub fn open(root: PathBuf, enabled: bool) -> anyhow::Result<Self> {
        if enabled {
            Self::check_version(&root)?;
            Self::init_dirs(&root)?;
        }
        Ok(Self { root, enabled })
    }

    /// Open the cache by walking upward from `start` to find the repo root.
    ///
    /// Combines [`root_from`] with [`ResultCache::open`].
    pub fn open_from(start: &Path, enabled: bool) -> anyhow::Result<Self> {
        Self::open(root_from(start), enabled)
    }

    /// Open the cache by walking upward from the current working directory.
    ///
    /// Combines [`root_from_cwd`] with [`ResultCache::open`].
    pub fn open_default(enabled: bool) -> anyhow::Result<Self> {
        Self::open(root_from_cwd()?, enabled)
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Validate the on-disk `VERSION` sentinel against [`CACHE_FORMAT_VERSION`].
    ///
    /// When a tree already exists (the `VERSION` file is present) and its
    /// contents do not match the current format version, return an error so the
    /// caller does not serve stale entries.  A missing sentinel (fresh tree) is
    /// not an error — [`init_dirs`] writes it.
    ///
    /// [`init_dirs`]: ResultCache::init_dirs
    fn check_version(root: &Path) -> anyhow::Result<()> {
        let version_path = root.join("VERSION");
        match std::fs::read_to_string(&version_path) {
            Ok(found) if found != CACHE_FORMAT_VERSION => Err(anyhow::anyhow!(
                "cache format version mismatch: expected {CACHE_FORMAT_VERSION}, found {found}; \
                 clear the cache or pass --no-cache"
            )),
            // Matching sentinel, or a missing one (fresh tree) — both fine.
            _ => Ok(()),
        }
    }

    /// Create the full sub-directory tree and write the VERSION sentinel.
    fn init_dirs(root: &Path) -> anyhow::Result<()> {
        for sub in ["results/lint", "results/fmt", "results/hook"] {
            std::fs::create_dir_all(root.join(sub)).map_err(|e| {
                anyhow::anyhow!(
                    "failed to create cache dir {}: {e}",
                    root.join(sub).display()
                )
            })?;
        }
        // Write the sentinel only for a fresh tree; `create_new` makes this
        // atomic, so a concurrent opener cannot race between an existence check
        // and the write. An already-present sentinel is left untouched.
        let version_path = root.join("VERSION");
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&version_path)
        {
            Ok(mut file) => {
                use std::io::Write as _;
                file.write_all(CACHE_FORMAT_VERSION.as_bytes())
                    .map_err(|e| {
                        anyhow::anyhow!(
                            "failed to write cache VERSION sentinel {}: {e}",
                            version_path.display()
                        )
                    })?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "failed to create cache VERSION sentinel {}: {e}",
                    version_path.display()
                ));
            }
        }
        Ok(())
    }

    /// Return the on-disk path for a cache entry.
    fn entry_path(&self, namespace: Namespace, key: &CacheKey) -> PathBuf {
        self.root
            .join("results")
            .join(namespace.as_dir())
            .join(key.as_str())
    }

    // -----------------------------------------------------------------------
    // Digest construction
    // -----------------------------------------------------------------------

    /// Compute an [`InputDigest`] for a single file's text content.
    ///
    /// This is the single-file convenience used for lint and format results;
    /// it is equivalent to [`file_set_digest`] with a set containing one entry
    /// whose path component is the empty string.
    ///
    /// [`file_set_digest`]: ResultCache::file_set_digest
    pub fn single_file_digest(content: &str) -> InputDigest {
        Self::file_set_digest(std::iter::once(("", content.as_bytes())))
    }

    /// Compute an [`InputDigest`] over a set of `(repo_relative_path, bytes)` pairs.
    ///
    /// Algorithm:
    /// 1. Compute `blake3(bytes)` for each file.
    /// 2. Sort entries by path (byte order) for a stable digest.
    /// 3. Feed the outer hasher with `path \0 file_hash_raw_bytes` for each entry.
    ///
    /// For hooks, pass every file in the hook's matched input set.  For a
    /// single lint/fmt file use [`single_file_digest`] instead.
    ///
    /// [`single_file_digest`]: ResultCache::single_file_digest
    pub fn file_set_digest<'a>(files: impl Iterator<Item = (&'a str, &'a [u8])>) -> InputDigest {
        let mut entries: Vec<(&'a str, blake3::Hash)> = files
            .map(|(path, bytes)| (path, blake3::hash(bytes)))
            .collect();
        // Sort by path so the digest is stable regardless of iteration order.
        entries.sort_unstable_by_key(|(path, _)| *path);

        let mut outer = blake3::Hasher::new();
        for (path, hash) in &entries {
            outer.update(path.as_bytes());
            outer.update(b"\0");
            outer.update(hash.as_bytes());
        }
        InputDigest(outer.finalize().to_hex().to_string())
    }

    // -----------------------------------------------------------------------
    // Key derivation
    // -----------------------------------------------------------------------

    /// Derive a [`CacheKey`] for a cache entry.
    ///
    /// The key is blake3 over:
    ///
    /// ```text
    /// namespace_dir \0 id \0 version \0 toml(args) \0 input_digest
    /// ```
    ///
    /// - `namespace` — selects the storage sub-directory.
    /// - `id` — engine name (lint/fmt) or hook id.
    /// - `version` — engine or hook version string; **must change whenever
    ///   output could change** since it is part of the cache key.
    /// - `args` — a TOML table; for engines this is `EngineConfig.options`,
    ///   for hooks it is the declared env + args table.
    /// - `input_digest` — content fingerprint from [`single_file_digest`] or
    ///   [`file_set_digest`].
    ///
    /// [`single_file_digest`]: ResultCache::single_file_digest
    /// [`file_set_digest`]: ResultCache::file_set_digest
    pub fn key(
        namespace: Namespace,
        id: &str,
        version: &str,
        args: &toml::Table,
        input_digest: &InputDigest,
    ) -> CacheKey {
        let mut hasher = blake3::Hasher::new();
        hasher.update(namespace.as_dir().as_bytes());
        hasher.update(b"\0");
        hasher.update(id.as_bytes());
        hasher.update(b"\0");
        hasher.update(version.as_bytes());
        hasher.update(b"\0");
        let serialised_args = toml::to_string(args)
            .expect("cache: failed to serialize args toml::Table — this is a bug");
        hasher.update(serialised_args.as_bytes());
        hasher.update(b"\0");
        hasher.update(input_digest.as_str().as_bytes());
        CacheKey(hasher.finalize().to_hex().to_string())
    }

    // -----------------------------------------------------------------------
    // Storage
    // -----------------------------------------------------------------------

    /// Fetch a cached entry by key, or `None` on miss / when disabled.
    pub fn get(&self, namespace: Namespace, key: &CacheKey) -> Option<Vec<u8>> {
        if !self.enabled {
            return None;
        }
        std::fs::read(self.entry_path(namespace, key)).ok()
    }

    /// Store `bytes` under `key` with an atomic sibling-tmp + rename.
    ///
    /// No-op when the cache is disabled.
    pub fn put(&self, namespace: Namespace, key: &CacheKey, bytes: &[u8]) -> anyhow::Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let dest = self.entry_path(namespace, key);
        let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = self
            .root
            .join("results")
            .join(namespace.as_dir())
            .join(format!(
                ".{}.{}.{}.tmp",
                key.as_str(),
                std::process::id(),
                n
            ));
        std::fs::write(&tmp, bytes)
            .map_err(|e| anyhow::anyhow!("cache write {}: {e}", tmp.display()))?;
        if let Err(e) = std::fs::rename(&tmp, &dest) {
            // Don't leave the orphaned tmp file behind; ignore the cleanup error.
            let _ = std::fs::remove_file(&tmp);
            return Err(anyhow::anyhow!("cache rename to {}: {e}", dest.display()));
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Accessors
    // -----------------------------------------------------------------------

    /// Whether this cache is enabled.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// The cache root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use tempfile::TempDir;

    use super::*;

    /// Open an enabled cache rooted at an explicit temporary directory, so
    /// tests are isolated from the process cwd and any real `.git` tree.
    fn cache_at(dir: &TempDir) -> ResultCache {
        let root = dir.path().join("cache");
        ResultCache::open(root, true).expect("open cache")
    }

    fn empty_args() -> toml::Table {
        toml::Table::new()
    }

    // -----------------------------------------------------------------------
    // get / put round-trips
    // -----------------------------------------------------------------------

    #[test]
    fn get_returns_stored_bytes_on_hit() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = cache_at(&tmp);
        let digest = ResultCache::single_file_digest("content");
        let key = ResultCache::key(Namespace::Lint, "eng", "1", &empty_args(), &digest);
        cache.put(Namespace::Lint, &key, b"stored").unwrap();
        assert_eq!(
            cache.get(Namespace::Lint, &key).as_deref(),
            Some(&b"stored"[..])
        );
    }

    #[test]
    fn miss_when_content_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = cache_at(&tmp);
        let d1 = ResultCache::single_file_digest("content");
        let key1 = ResultCache::key(Namespace::Lint, "eng", "1", &empty_args(), &d1);
        cache.put(Namespace::Lint, &key1, b"stored").unwrap();
        let d2 = ResultCache::single_file_digest("different content");
        let key2 = ResultCache::key(Namespace::Lint, "eng", "1", &empty_args(), &d2);
        assert_ne!(key1, key2, "content change must alter key");
        assert_eq!(cache.get(Namespace::Lint, &key2), None);
    }

    #[test]
    fn miss_when_version_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = cache_at(&tmp);
        let digest = ResultCache::single_file_digest("content");
        let key1 = ResultCache::key(Namespace::Lint, "eng", "1", &empty_args(), &digest);
        cache.put(Namespace::Lint, &key1, b"stored").unwrap();
        let key2 = ResultCache::key(Namespace::Lint, "eng", "2", &empty_args(), &digest);
        assert_ne!(key1, key2, "version change must alter key");
        assert_eq!(cache.get(Namespace::Lint, &key2), None);
    }

    #[test]
    fn miss_when_args_change() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = cache_at(&tmp);
        let digest = ResultCache::single_file_digest("content");
        let args_a = empty_args();
        let mut args_b = empty_args();
        args_b.insert("line-length".into(), toml::Value::Integer(120));
        let key1 = ResultCache::key(Namespace::Lint, "eng", "1", &args_a, &digest);
        cache.put(Namespace::Lint, &key1, b"stored").unwrap();
        let key2 = ResultCache::key(Namespace::Lint, "eng", "1", &args_b, &digest);
        assert_ne!(key1, key2, "args change must alter key");
        assert_eq!(cache.get(Namespace::Lint, &key2), None);
    }

    #[test]
    fn disabled_cache_is_a_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("cache");
        let cache = ResultCache::open(root.clone(), false).unwrap();
        let digest = ResultCache::single_file_digest("content");
        let key = ResultCache::key(Namespace::Lint, "eng", "1", &empty_args(), &digest);
        cache.put(Namespace::Lint, &key, b"stored").unwrap();
        assert_eq!(
            cache.get(Namespace::Lint, &key),
            None,
            "disabled get must miss"
        );
        assert!(!root.exists(), "disabled put must not create cache dir");
    }

    // -----------------------------------------------------------------------
    // Namespace segregation
    // -----------------------------------------------------------------------

    #[test]
    fn namespace_segregates_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = cache_at(&tmp);
        let digest = ResultCache::single_file_digest("x");
        let args = empty_args();
        let lint_key = ResultCache::key(Namespace::Lint, "eng", "1", &args, &digest);
        let fmt_key = ResultCache::key(Namespace::Fmt, "eng", "1", &args, &digest);
        let hook_key = ResultCache::key(Namespace::Hook, "eng", "1", &args, &digest);
        // Keys themselves differ because the namespace component is included.
        let keys: HashSet<_> = [lint_key.as_str(), fmt_key.as_str(), hook_key.as_str()]
            .into_iter()
            .collect();
        assert_eq!(keys.len(), 3, "each namespace must produce a distinct key");
        // Writing to lint does not satisfy fmt or hook.
        cache.put(Namespace::Lint, &lint_key, b"lint").unwrap();
        assert_eq!(cache.get(Namespace::Fmt, &fmt_key), None);
        assert_eq!(cache.get(Namespace::Hook, &hook_key), None);
    }

    // -----------------------------------------------------------------------
    // InputDigest — single-file vs file-set consistency
    // -----------------------------------------------------------------------

    #[test]
    fn single_file_digest_matches_file_set_of_one_with_empty_path() {
        let content = "hello world";
        let single = ResultCache::single_file_digest(content);
        let set = ResultCache::file_set_digest(std::iter::once(("", content.as_bytes())));
        assert_eq!(
            single, set,
            "single_file_digest must equal file_set_digest({{'', content}})"
        );
    }

    #[test]
    fn file_set_digest_is_path_order_stable() {
        let a = ("alpha.py", b"content_a" as &[u8]);
        let b = ("beta.py", b"content_b" as &[u8]);
        let forward = ResultCache::file_set_digest([a, b].into_iter());
        let backward = ResultCache::file_set_digest([b, a].into_iter());
        assert_eq!(
            forward, backward,
            "file_set_digest must be stable across input order"
        );
    }

    #[test]
    fn file_set_digest_differs_on_content_change() {
        let d1 =
            ResultCache::file_set_digest([("a.py", b"v1" as &[u8]), ("b.py", b"v2")].into_iter());
        let d2 = ResultCache::file_set_digest(
            [("a.py", b"v1" as &[u8]), ("b.py", b"CHANGED")].into_iter(),
        );
        assert_ne!(d1, d2);
    }

    // -----------------------------------------------------------------------
    // Anchor walk
    // -----------------------------------------------------------------------

    #[test]
    fn find_anchor_returns_nearest_ancestor_with_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let deep = root.join("a").join("b");
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(find_anchor(&deep, ".git").as_deref(), Some(root));
    }

    #[test]
    fn find_anchor_returns_none_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let deep = tmp.path().join("x").join("y");
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(find_anchor(&deep, ".git"), None);
    }

    #[test]
    fn root_from_prefers_git_over_poly_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // .git at root, poly.toml deeper
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let pkg = root.join("pkg");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("poly.toml"), b"").unwrap();
        let cache_root = root_from(&pkg);
        // Should anchor at root (the .git anchor), not at pkg
        assert_eq!(
            cache_root,
            root.join(".polylint").join("cache"),
            ".git anchor must win over poly.toml"
        );
    }

    #[test]
    fn root_from_falls_back_to_poly_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("poly.toml"), b"").unwrap();
        let deep = root.join("sub");
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(root_from(&deep), root.join(".polylint").join("cache"));
    }

    // -----------------------------------------------------------------------
    // VERSION sentinel
    // -----------------------------------------------------------------------

    #[test]
    fn version_sentinel_is_written_on_open() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("cache");
        ResultCache::open(root.clone(), true).unwrap();
        let version = std::fs::read_to_string(root.join("VERSION")).unwrap();
        assert_eq!(version, CACHE_FORMAT_VERSION);
    }

    #[test]
    fn version_sentinel_not_overwritten_when_present() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("cache");
        // Pre-create with the current (matching) version string.
        std::fs::create_dir_all(root.join("results/lint")).unwrap();
        std::fs::create_dir_all(root.join("results/fmt")).unwrap();
        std::fs::create_dir_all(root.join("results/hook")).unwrap();
        std::fs::write(root.join("VERSION"), CACHE_FORMAT_VERSION).unwrap();
        ResultCache::open(root.clone(), true).unwrap();
        // `create_new` leaves an existing sentinel untouched.
        let version = std::fs::read_to_string(root.join("VERSION")).unwrap();
        assert_eq!(
            version, CACHE_FORMAT_VERSION,
            "existing VERSION must not be overwritten"
        );
    }

    #[test]
    fn open_fails_on_version_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("cache");
        // Pre-create a tree carrying a stale, mismatched sentinel.
        std::fs::create_dir_all(root.join("results/lint")).unwrap();
        std::fs::create_dir_all(root.join("results/fmt")).unwrap();
        std::fs::create_dir_all(root.join("results/hook")).unwrap();
        std::fs::write(root.join("VERSION"), "0").unwrap();
        let err = ResultCache::open(root.clone(), true)
            .expect_err("open must fail when the on-disk VERSION does not match");
        let message = err.to_string();
        assert!(
            message.contains("cache format version mismatch"),
            "error should explain the mismatch, got: {message}"
        );
        // The stale sentinel is left untouched for the user to clear.
        let version = std::fs::read_to_string(root.join("VERSION")).unwrap();
        assert_eq!(version, "0");
    }
}
