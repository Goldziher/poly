//! `[cache]` configuration table from `poly.toml`.
//!
//! The `[cache]` table is **fully optional**: all fields carry defaults so an
//! absent table is equivalent to the defaults shown below.
//!
//! ```toml
//! [cache]
//! enabled = true           # master enable/disable switch (default true)
//! # dir = "..."            # optional repo-local root override; absent → anchor walk
//!
//! [cache.results]
//! hooks = "safe"           # Off | Safe (default) | Aggressive
//!
//! [cache.sccache]
//! enabled = false          # opt-in; off by default
//! # bin = "sccache"
//! # dir = "~/.sccache"
//! # max_size = "10G"
//! ```

use serde::Deserialize;

// ---------------------------------------------------------------------------
// HookCacheMode
// ---------------------------------------------------------------------------

/// Controls when hook results are served from the result cache.
///
/// The default is [`Safe`] — hook results are cached only when the hook
/// explicitly declares its inputs via `cache = { inputs = [...] }` in the
/// job definition.
///
/// [`Safe`]: HookCacheMode::Safe
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum HookCacheMode {
    /// Never cache hook results.
    Off,
    /// Cache hook results only when the hook declares its inputs (default).
    ///
    /// Builtins (`polylint`/`polyfmt`) are always cached by matched files
    /// because their footprint equals their input set.  An inline command is
    /// cached only when it carries a `cache = { inputs = [...] }` declaration;
    /// otherwise it always reruns.
    #[default]
    Safe,
    /// Cache based on matched files only, regardless of declared inputs.
    ///
    /// May produce stale results for commands whose behaviour depends on
    /// inputs outside the matched file set (e.g. environment variables,
    /// generated files).  **Use with care.**
    Aggressive,
}

// ---------------------------------------------------------------------------
// ResultsCacheConfig
// ---------------------------------------------------------------------------

/// Configuration for the tier-1 result cache.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ResultsCacheConfig {
    /// Cache mode for hook results.
    pub hooks: HookCacheMode,
}

// ---------------------------------------------------------------------------
// SccacheConfig
// ---------------------------------------------------------------------------

/// Configuration for the tier-2 sccache integration (opt-in, off by default).
///
/// When enabled, `poly hooks` starts the shared sccache server (idempotent,
/// long-lived) before running hooks, and injects `RUSTC_WRAPPER` /
/// `SCCACHE_DIR` / `SCCACHE_CACHE_SIZE` only into hooks that declare
/// `cache.compiler = true`.
///
/// Sccache is **not implemented in this crate** — this is only the config
/// surface.  Wire-up happens in `crates/poly-hooks` (Workstream B/C).
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct SccacheConfig {
    /// Enable sccache for compiler hooks (default `false`).
    pub enabled: bool,
    /// Path to the `sccache` binary.  When absent, `$PATH` is searched.
    pub bin: Option<String>,
    /// Override sccache storage directory.
    pub dir: Option<String>,
    /// Maximum cache size string understood by sccache (e.g. `"10G"`).
    pub max_size: Option<String>,
}

// ---------------------------------------------------------------------------
// CacheConfig
// ---------------------------------------------------------------------------

fn default_cache_enabled() -> bool {
    true
}

/// Configuration for the `[cache]` table in `poly.toml`.
///
/// All fields default so `[cache]` is entirely optional.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CacheConfig {
    /// Master enable/disable switch for both cache tiers (default `true`).
    ///
    /// Equivalent to `--no-cache` on the CLI.
    #[serde(default = "default_cache_enabled")]
    pub enabled: bool,
    /// Override the repo-local cache root directory.
    ///
    /// When absent the default anchor walk applies (see
    /// `poly_cache::root_from`): nearest `.git` ancestor →
    /// `<that>/.polylint/cache`.
    pub dir: Option<String>,
    /// Tier-1 result-cache configuration.
    pub results: ResultsCacheConfig,
    /// Tier-2 sccache configuration.
    pub sccache: SccacheConfig,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            enabled: default_cache_enabled(),
            dir: None,
            results: ResultsCacheConfig::default(),
            sccache: SccacheConfig::default(),
        }
    }
}
