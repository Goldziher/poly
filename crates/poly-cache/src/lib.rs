//! Unified result-cache for `poly lint`, `poly fmt`, and `poly hooks`.
//!
//! # Overview
//!
//! [`ResultCache`] is a blake3 content-hash result cache backed by files under a
//! per-user cache directory (`<platform-cache>/poly/<repo-key>/`, see
//! [`repo_cache_dir`]). It generalises the single-file key that `poly-core` uses
//! for lint and format results into a **file-set digest** ([`InputDigest`]),
//! enabling multi-file caching for hook results without changing the single-file
//! path.
//!
//! # Storage layout
//!
//! Every directory below is created owner-only (`0700`) on Unix — the staged
//! snapshot mirrors the repository's source, so the tree must not be left
//! world-readable by an inherited umask. See [`permissions`] for the full
//! rationale and for what poly deliberately leaves alone.
//!
//! ```text
//! <platform-cache>/poly/<repo-key>/
//!   VERSION              — format-version sentinel; bump CACHE_FORMAT_VERSION on
//!                          breaking layout changes so GC can detect stale trees
//!   .lock                — advisory lock PLACEHOLDER for GC/clean ops; routine
//!                          get/put use atomic sibling-tmp + rename instead (see
//!                          ADVISORY_LOCK_NOTE below)
//!   results/
//!     lint/<hex-key>     — serde_json-encoded Vec<Diagnostic>
//!     fmt/<hex-key>      — UTF-8 formatted text
//!     hook/<hex-key>     — JSON-encoded hook outcome
//!
//! <platform-cache>/poly/hook-sources/
//!   <url-key>/mirror.git/             — shared bare Git mirror
//!   <url-key>/source.lock             — per-source interprocess lock
//!   <url-key>/checkouts/<commit-sha>/ — immutable producer checkout
//! ```
//!
//! # Key derivation
//!
//! ```text
//! input_digest  = blake3( concat(path \0 blake3(file_bytes)_raw  for each file, sorted by path) )
//!
//! cache_key     = blake3( format_version \0 build_identity \0 namespace_dir \0 id \0 version
//!                         \0 toml(args) \0 input_digest_hex )
//! ```
//!
//! The preamble is a layered identity, outermost first:
//!
//! | Layer            | Source                                | Invalidates when                       |
//! |------------------|---------------------------------------|----------------------------------------|
//! | `format_version` | [`CACHE_FORMAT_VERSION`]              | what is *stored* changes shape         |
//! | `build_identity` | [`poly_buildinfo::cache_identity`]    | the poly binary itself changes         |
//! | `id` + `version` | `Engine::name` / `Engine::version`    | a wrapped upstream crate is bumped     |
//! | `args`           | resolved engine config                | the user reconfigures the engine       |
//! | `input_digest`   | file bytes (+ path for lint)          | the file changes                       |
//!
//! Only `build_identity` is automatic — it is what makes the cache correct by
//! construction when *poly's own* logic changes, without anyone remembering to
//! bump a string. See [`ResultCache::key`].
//!
//! For a single file use [`ResultCache::single_file_digest`]; for a matched hook file
//! set use [`ResultCache::file_set_digest`].
//!
//! # Adoption path for `poly-core/src/runner.rs`
//!
//! The migration is a near one-line swap per call site.
//!
//! **Before** (using the private `poly_core::cache::Cache`):
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

mod maintenance;
pub mod permissions;

pub use maintenance::{CacheStats, NamespaceStats};
pub use permissions::{create_dir_all_private, ensure_private_dir};

/// On-disk format version written to the `VERSION` sentinel file.
///
/// Increment this whenever the cache layout changes incompatibly.  Tools such
/// as `poly cache gc` compare the sentinel against this value to decide whether
/// an existing tree is safe to reuse.
///
/// Bumped to `4` when the running binary's build identity joined the key
/// preamble: every pre-existing entry became unreachable in one step, and the
/// bump reclaims that dead weight on the first run instead of leaving it for a
/// later `gc`.
pub const CACHE_FORMAT_VERSION: &str = "4";

/// The running binary's build identity, folded into every result-cache key.
///
/// An engine's `version()` only moves when that engine or its wrapped upstream
/// crate is hand-bumped — but a *different build of poly* can change an
/// engine's output without moving `version()` at all (a tweak to the generic
/// tree-sitter reindent logic, a changed default, an unreleased fix). Every
/// such build still calls itself `0.19.7`, so a version-only preamble lets one
/// build read another's entries and serve a verdict its own code would never
/// have produced — including a stale "already formatted" for a file it would
/// rewrite.
///
/// [`poly_buildinfo::cache_identity`] is the narrowest string two binaries may
/// share and still be trusted with each other's results: a version for tagged
/// release builds (so `v0.19.7` on every machine shares one cache), and
/// version + commit + profile + executable fingerprint for anything else.
fn build_identity() -> &'static str {
    poly_buildinfo::cache_identity()
}

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Environment variable overriding the poly cache home (`<platform-cache>/poly`).
/// When set, its value replaces the OS cache directory as the base for every
/// per-repo cache slot. Intended for CI isolation and test sandboxing.
pub const CACHE_HOME_ENV: &str = "POLY_CACHE_HOME";

/// Legacy in-repo cache directory (poly ≤ 0.8), removed on sight during
/// migration now that the cache lives under the per-user cache home.
const LEGACY_CACHE_DIR: &str = ".polylint";

/// Return the nearest ancestor of `start` (inclusive) that contains a
/// filesystem entry named `marker`, or `None` if no ancestor does.
///
/// Used by [`repo_anchor`] to locate the repository root.
pub fn find_anchor(start: &Path, marker: &str) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|dir| dir.join(marker).exists())
        .map(Path::to_path_buf)
}

/// Resolve the repository root for `start`: the nearest `.git` ancestor, else
/// the nearest `poly.toml` ancestor, else `start` itself.
///
/// The `.git` anchor wins even when a config file sits deeper, so the cache is
/// shared across a repository rather than fragmented per sub-package.
pub fn repo_anchor(start: &Path) -> PathBuf {
    find_anchor(start, ".git")
        .or_else(|| find_anchor(start, "poly.toml"))
        .unwrap_or_else(|| start.to_path_buf())
}

/// The per-user poly cache home — `<platform-cache>/poly`.
///
/// Honors [`CACHE_HOME_ENV`] when set; otherwise resolves the OS cache
/// directory via `etcetera` (`~/.cache` on Linux / `$XDG_CACHE_HOME`,
/// `~/Library/Caches` on macOS, `%LOCALAPPDATA%` on Windows).
pub fn cache_home() -> anyhow::Result<PathBuf> {
    if let Some(dir) = std::env::var_os(CACHE_HOME_ENV) {
        return Ok(PathBuf::from(dir));
    }
    use etcetera::BaseStrategy;
    let strategy = etcetera::choose_base_strategy()
        .map_err(|e| anyhow::anyhow!("could not resolve the platform cache directory: {e}"))?;
    Ok(strategy.cache_dir().join("poly"))
}

/// Global cache root for shared Git hook sources.
///
/// Unlike result caches, hook sources are keyed by their exact remote URL
/// and shared by every consumer repository on the machine.
pub fn hook_sources_dir() -> anyhow::Result<PathBuf> {
    Ok(cache_home()?.join("hook-sources"))
}

/// Stable key for a Git hook source URL.
///
/// A thin alias for [`remote_source_key`]; hook sources and other remote-git
/// sources share one URL-keying algorithm so their on-disk caches stay
/// interoperable.
pub fn hook_source_key(url: &str) -> String {
    remote_source_key(url)
}

/// Global cache root for shared remote-git sources (e.g. config `extends`).
///
/// Sibling to [`hook_sources_dir`]: a separate `sources/` directory keyed by
/// remote URL, shared by every consumer repository on the machine. Kept
/// distinct from `hook-sources/` so the two caches never orphan each other.
pub fn remote_sources_dir() -> anyhow::Result<PathBuf> {
    Ok(cache_home()?.join("sources"))
}

/// Stable, filesystem-safe key for a remote-git source URL.
///
/// The first 32 hex chars of `blake3(url)`. Shared with [`hook_source_key`] so
/// the same URL maps to the same cache slot regardless of which caller keyed it.
pub fn remote_source_key(url: &str) -> String {
    blake3::hash(url.as_bytes()).to_hex()[..32].to_string()
}

/// A stable, filesystem-safe key for the repository rooted at `anchor`.
///
/// The first 16 hex chars of `blake3(canonical(anchor))` — canonicalizing the
/// root path so the same repository maps to the same cache slot regardless of
/// the working directory (or symlink) poly was invoked through.
pub fn repo_key(anchor: &Path) -> String {
    let canonical = dunce::canonicalize(anchor).unwrap_or_else(|_| anchor.to_path_buf());
    let digest = blake3::hash(canonical.to_string_lossy().as_bytes());
    digest.to_hex()[..16].to_string()
}

/// The per-repo cache directory for the repository containing `start`:
/// `<cache_home>/<repo-key>`. Result-cache entries live under `results/` here
/// and the hook staged snapshot under `staged/`.
///
/// Two best-effort side effects, so callers get a directory that is safe to
/// write into rather than a bare path:
///
/// - any legacy in-repo `.polylint/` directory is removed, so an upgrade cleans
///   up after the previous layout;
/// - the directory itself is created owner-only ([`ensure_private_dir`]). This
///   is the choke point every consumer resolves through, including the hook
///   staged snapshot, so creating it here means a snapshot of the repository's
///   source can never land in a directory other local users can traverse — even
///   when the result cache is disabled and never creates its own tree.
///
/// Creation failures are logged and ignored: this resolver has always returned
/// a path, and the caller that actually writes reports the error with its own
/// context.
pub fn repo_cache_dir(start: &Path) -> anyhow::Result<PathBuf> {
    let dir = resolve_repo_cache_dir(start)?;
    if let Err(error) = permissions::ensure_private_dir(&dir) {
        tracing::debug!(dir = %dir.display(), "could not pre-create the cache directory: {error}");
    }
    Ok(dir)
}

/// Resolve the per-repo cache directory without creating it.
///
/// The path half of [`repo_cache_dir`], for the paths that must not materialize
/// a cache tree — a disabled cache creates nothing.
fn resolve_repo_cache_dir(start: &Path) -> anyhow::Result<PathBuf> {
    let anchor = repo_anchor(start);
    remove_legacy_cache(&anchor);
    Ok(cache_home()?.join(repo_key(&anchor)))
}

/// Best-effort removal of a legacy in-repo `.polylint/` cache directory left by
/// poly ≤ 0.8. Errors are ignored — this is migration hygiene, never fatal.
pub fn remove_legacy_cache(anchor: &Path) {
    let legacy = anchor.join(LEGACY_CACHE_DIR);
    if legacy.exists() {
        let _ = std::fs::remove_dir_all(&legacy);
    }
}

/// Resolve the result-cache root for `start` — equivalent to [`repo_cache_dir`].
/// The [`ResultCache`] stores its `results/` tree and `VERSION` sentinel here.
pub fn root_from(start: &Path) -> anyhow::Result<PathBuf> {
    repo_cache_dir(start)
}

/// Resolve the result-cache root from the current working directory.
///
/// Equivalent to `root_from(&std::env::current_dir()?)`.
pub fn root_from_cwd() -> anyhow::Result<PathBuf> {
    let cwd = std::env::current_dir().map_err(|e| anyhow::anyhow!("could not read current directory: {e}"))?;
    root_from(&cwd)
}

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
    /// Every namespace, in a fixed order — used by maintenance operations that
    /// walk the whole `results/` tree.
    pub const ALL: [Namespace; 3] = [Namespace::Lint, Namespace::Fmt, Namespace::Hook];

    /// The sub-directory component used in the storage path.
    pub fn as_dir(self) -> &'static str {
        match self {
            Namespace::Lint => "lint",
            Namespace::Fmt => "fmt",
            Namespace::Hook => "hook",
        }
    }
}

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

/// The TOML serialisation of an engine's (or hook's) `args` table, computed
/// once via [`ResultCache::serialize_args`] and reused across many per-file
/// keys via [`ResultCache::key_with_args`].
///
/// Serialising `args` is comparatively expensive, so the per-file rayon path
/// serialises once per engine and borrows the result into the file loop rather
/// than re-serialising for every file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerializedArgs(String);

impl SerializedArgs {
    /// The serialised TOML string folded into a [`CacheKey`].
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A content-hash result cache backed by files under
/// `<platform-cache>/poly/<repo-key>/` (see [`repo_cache_dir`]).
///
/// `ResultCache` is `Send + Sync`: individual puts are atomic (sibling-tmp +
/// rename) so concurrent rayon workers never read a torn file.
///
/// # Disabled mode
///
/// When constructed with `enabled = false`, every `get` returns `None` and
/// every `put` is a no-op.  The directory is not created.
///
/// `Clone` duplicates the lightweight handle (root path + enabled flag); both
/// clones address the same on-disk cache directory.
#[derive(Debug, Clone)]
pub struct ResultCache {
    /// `<platform-cache>/poly/<repo-key>/`
    root: PathBuf,
    enabled: bool,
}

impl ResultCache {
    /// Open the cache at an explicit `root` directory.
    ///
    /// When `enabled`, creates the full sub-directory tree and writes the
    /// `VERSION` sentinel.  When disabled, returns a no-op stub.
    ///
    /// This is the low-level, non-healing open used by the `poly cache`
    /// maintenance commands, so introspection (`stats` / `size`) is read-only and
    /// never wipes a stale-layout tree. The run paths use [`open_from`] /
    /// [`open_default`], which self-heal first.
    ///
    /// [`open_from`]: ResultCache::open_from
    /// [`open_default`]: ResultCache::open_default
    pub fn open(root: PathBuf, enabled: bool) -> anyhow::Result<Self> {
        let cache = Self { root, enabled };
        if enabled {
            Self::init_dirs(&cache.root)?;
        }
        Ok(cache)
    }

    /// Open the cache, first wiping the entry tree if the on-disk `VERSION`
    /// sentinel is from an incompatible layout (self-healing, not only under
    /// `cache gc`). This is the run-path open: a `poly lint`/`fmt`/`hooks`
    /// invocation self-heals a stale cache before reading or writing results,
    /// and sweeps stranded entries at most once a day.
    ///
    /// A failed sweep is reported and ignored: eviction is hygiene, and losing
    /// disk space is not a reason to fail a lint run.
    fn open_healed(root: PathBuf, enabled: bool) -> anyhow::Result<Self> {
        let cache = Self { root, enabled };
        if enabled {
            cache.heal_stale_layout()?;
            Self::init_dirs(&cache.root)?;
            match cache.sweep_if_due() {
                Ok(0) => {}
                Ok(freed) => tracing::debug!(freed, "swept stranded result-cache entries"),
                Err(error) => tracing::warn!("automatic result-cache sweep failed: {error:#}"),
            }
        }
        Ok(cache)
    }

    /// Open the cache by walking upward from `start` to find the repo root.
    ///
    /// Combines [`root_from`] with the self-healing [`open_healed`]. An enabled
    /// cache creates its root owner-only; a disabled one only resolves the path.
    ///
    /// [`open_healed`]: ResultCache::open_healed
    pub fn open_from(start: &Path, enabled: bool) -> anyhow::Result<Self> {
        Self::open_healed(Self::resolve_root(start, enabled)?, enabled)
    }

    /// Resolve the cache root for `start`, creating it only when the cache is
    /// enabled — a disabled cache is a no-op that materializes nothing, so it
    /// takes the non-creating half of [`repo_cache_dir`].
    fn resolve_root(start: &Path, enabled: bool) -> anyhow::Result<PathBuf> {
        if enabled {
            root_from(start)
        } else {
            resolve_repo_cache_dir(start)
        }
    }

    /// Open the cache by walking upward from the current working directory.
    ///
    /// [`open_from`] rooted at the current directory, so it inherits the same
    /// self-healing open and the same "a disabled cache creates nothing" rule.
    ///
    /// [`open_from`]: ResultCache::open_from
    pub fn open_default(enabled: bool) -> anyhow::Result<Self> {
        let cwd = std::env::current_dir().map_err(|e| anyhow::anyhow!("could not read current directory: {e}"))?;
        Self::open_from(&cwd, enabled)
    }

    /// Create the full sub-directory tree and write the VERSION sentinel.
    ///
    /// Every directory is created owner-only on Unix (see [`permissions`]); an
    /// existing one keeps whatever mode it has, and only earns a warning.
    fn init_dirs(root: &Path) -> anyhow::Result<()> {
        permissions::ensure_private_dir(root)
            .map_err(|e| anyhow::anyhow!("failed to create cache dir {}: {e}", root.display()))?;
        for sub in ["results/lint", "results/fmt", "results/hook"] {
            permissions::create_dir_all_private(&root.join(sub))
                .map_err(|e| anyhow::anyhow!("failed to create cache dir {}: {e}", root.join(sub).display()))?;
        }
        let version_path = root.join("VERSION");
        if !version_path.exists() {
            std::fs::write(&version_path, CACHE_FORMAT_VERSION).map_err(|e| {
                anyhow::anyhow!("failed to write cache VERSION sentinel {}: {e}", version_path.display())
            })?;
        }
        Ok(())
    }

    /// Return the on-disk path for a cache entry.
    fn entry_path(&self, namespace: Namespace, key: &CacheKey) -> PathBuf {
        self.root.join("results").join(namespace.as_dir()).join(key.as_str())
    }

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

    /// Like [`single_file_digest`] but folds `path` into the digest. Use this for
    /// **lint** results, whose diagnostics can depend on the file's path or its
    /// on-disk package context (e.g. ruff's INP001 message embeds the path, and
    /// isort first-party classification depends on the package root). Without the
    /// path, two byte-identical files (e.g. empty `__init__.py`) would share a
    /// cache entry and the second would be served the first's path-bearing
    /// diagnostics. Format output is path-independent, so it keeps using
    /// [`single_file_digest`].
    ///
    /// [`single_file_digest`]: ResultCache::single_file_digest
    pub fn single_file_digest_with_path(path: &str, content: &str) -> InputDigest {
        Self::file_set_digest(std::iter::once((path, content.as_bytes())))
    }

    /// Compute an [`InputDigest`] over a set of `(repo_relative_path, bytes)` pairs.
    ///
    /// Algorithm:
    /// 1. Compute `blake3(bytes)` for each file.
    /// 2. Sort entries by path (byte order) for a stable digest.
    /// 3. Feed the outer hasher with `path \0 file_hash_raw` for each entry,
    ///    where `file_hash_raw` is the 32 raw hash bytes.
    ///
    /// The per-file hash is kept as a [`blake3::Hash`] (a `Copy`, stack-resident
    /// 32-byte value) and its raw bytes are fed straight into the outer hasher —
    /// no per-file hex `String` is allocated. This runs once per file on every
    /// lint/format pass, so the avoided allocation multiplies by the corpus size.
    ///
    /// For hooks, pass every file in the hook's matched input set.  For a
    /// single lint/fmt file use [`single_file_digest`] instead.
    ///
    /// [`single_file_digest`]: ResultCache::single_file_digest
    pub fn file_set_digest<'a>(files: impl Iterator<Item = (&'a str, &'a [u8])>) -> InputDigest {
        let mut entries: Vec<(&'a str, blake3::Hash)> =
            files.map(|(path, bytes)| (path, blake3::hash(bytes))).collect();
        entries.sort_unstable_by_key(|(path, _)| *path);

        let mut outer = blake3::Hasher::new();
        for (path, file_hash) in &entries {
            outer.update(path.as_bytes());
            outer.update(b"\0");
            outer.update(file_hash.as_bytes());
        }
        InputDigest(outer.finalize().to_hex().to_string())
    }

    /// Derive a [`CacheKey`] for a cache entry.
    ///
    /// The key is blake3 over:
    ///
    /// ```text
    /// format_version \0 build_identity \0 namespace_dir \0 id \0 version \0 toml(args) \0 input_digest
    /// ```
    ///
    /// - `format_version` — [`CACHE_FORMAT_VERSION`]; a change to what is
    ///   *stored* makes every existing entry unreachable, independently of the
    ///   `VERSION` sentinel that reclaims their bytes.
    /// - `build_identity` — the running binary's identity ([`build_identity`]);
    ///   a build never reads entries written by a build that may behave
    ///   differently, so no upgrade — and no rebuild — can serve stale output.
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
        Self::key_with_args(namespace, id, version, &Self::serialize_args(args), input_digest)
    }

    /// Serialise an `args` table once for reuse across many [`key_with_args`]
    /// calls.
    ///
    /// The result is byte-for-byte the serialisation [`key`] folds in, so
    /// `key(.., args, ..)` and `key_with_args(.., &serialize_args(args), ..)`
    /// produce identical [`CacheKey`]s.
    ///
    /// [`key`]: ResultCache::key
    /// [`key_with_args`]: ResultCache::key_with_args
    pub fn serialize_args(args: &toml::Table) -> SerializedArgs {
        SerializedArgs(toml::to_string(args).unwrap_or_default())
    }

    /// Derive a [`CacheKey`] from pre-serialised `args`.
    ///
    /// This is the single key-derivation code path; [`key`] is a thin wrapper
    /// that serialises `args` first.  In the per-file rayon loop, serialise the
    /// engine's args once with [`serialize_args`] and borrow the
    /// [`SerializedArgs`] into every call here so the cost is paid once per
    /// engine rather than once per file.
    ///
    /// [`key`]: ResultCache::key
    /// [`serialize_args`]: ResultCache::serialize_args
    pub fn key_with_args(
        namespace: Namespace,
        id: &str,
        version: &str,
        args: &SerializedArgs,
        input_digest: &InputDigest,
    ) -> CacheKey {
        Self::key_with_build_identity(build_identity(), namespace, id, version, args, input_digest)
    }

    /// [`key_with_args`] with the build identity supplied explicitly.
    ///
    /// The public path always passes [`build_identity`]; this seam lets a test
    /// vary it to prove the identity participates in the key, which a running
    /// test process cannot do by rebuilding itself.
    ///
    /// [`key_with_args`]: ResultCache::key_with_args
    fn key_with_build_identity(
        build_identity: &str,
        namespace: Namespace,
        id: &str,
        version: &str,
        args: &SerializedArgs,
        input_digest: &InputDigest,
    ) -> CacheKey {
        let mut hasher = blake3::Hasher::new();
        hasher.update(CACHE_FORMAT_VERSION.as_bytes());
        hasher.update(b"\0");
        hasher.update(build_identity.as_bytes());
        hasher.update(b"\0");
        hasher.update(namespace.as_dir().as_bytes());
        hasher.update(b"\0");
        hasher.update(id.as_bytes());
        hasher.update(b"\0");
        hasher.update(version.as_bytes());
        hasher.update(b"\0");
        hasher.update(args.as_str().as_bytes());
        hasher.update(b"\0");
        hasher.update(input_digest.as_str().as_bytes());
        CacheKey(hasher.finalize().to_hex().to_string())
    }

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
        let tmp = self.root.join("results").join(namespace.as_dir()).join(format!(
            ".{}.{}.{}.tmp",
            key.as_str(),
            std::process::id(),
            n
        ));
        std::fs::write(&tmp, bytes).map_err(|e| anyhow::anyhow!("cache write {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &dest).map_err(|e| anyhow::anyhow!("cache rename to {}: {e}", dest.display()))?;
        Ok(())
    }

    /// Whether this cache is enabled.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// The cache root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[cfg(test)]
mod tests;
