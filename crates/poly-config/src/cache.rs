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

use std::path::Path;

use serde::Deserialize;

/// Default `sccache` binary name, resolved on `$PATH` when `bin` is unset.
pub const DEFAULT_SCCACHE_BIN: &str = "sccache";

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
    /// Builtins (`lint`/`fmt`) are always cached by matched files
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

impl SccacheConfig {
    /// Resolve and validate the `sccache` binary to invoke as `RUSTC_WRAPPER`.
    ///
    /// Because a repository's checked-in `poly.toml` controls this value and it
    /// becomes `RUSTC_WRAPPER`, only two shapes are accepted:
    ///
    /// - a **bare command name** (no path separators) resolved on `$PATH`
    ///   — the default `sccache` when `bin` is unset; or
    /// - an **absolute path**.
    ///
    /// A relative path containing separators (e.g. `./evil`, `a/b`) is rejected
    /// so a hostile repo cannot silently point the compiler wrapper at a
    /// repo-relative binary. The consumer (`poly-hooks` runner) MUST call this
    /// rather than reading [`bin`][Self::bin] directly.
    ///
    /// # Errors
    ///
    /// Returns `Err` when `bin` is empty or is a relative path with separators.
    pub fn validated_bin(&self) -> anyhow::Result<&str> {
        let Some(bin) = self.bin.as_deref() else {
            return Ok(DEFAULT_SCCACHE_BIN);
        };
        if bin.is_empty() {
            anyhow::bail!("[cache.sccache] bin must not be empty");
        }
        // A bare command name has exactly one path component and no separator of
        // either platform's flavour (guard both so a Windows-style separator is
        // rejected on Unix too).
        let is_bare_name = Path::new(bin).components().count() == 1 && !bin.contains('/') && !bin.contains('\\');
        if Path::new(bin).is_absolute() || is_bare_name {
            Ok(bin)
        } else {
            anyhow::bail!(
                "[cache.sccache] bin = {bin:?} must be a bare command name (resolved on \
                 $PATH) or an absolute path; a relative path with separators is rejected \
                 to avoid executing a repo-relative binary as RUSTC_WRAPPER"
            );
        }
    }
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
    /// Override the cache root directory.
    ///
    /// When absent the cache lives in the per-user cache home (see
    /// `poly_cache::root_from`): `<platform-cache>/poly/<repo-key>`, keyed by the
    /// nearest `.git` / `poly.toml` ancestor.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn with_bin(bin: Option<&str>) -> SccacheConfig {
        SccacheConfig {
            bin: bin.map(str::to_owned),
            ..SccacheConfig::default()
        }
    }

    #[test]
    fn validated_bin_defaults_to_sccache_when_unset() {
        assert_eq!(with_bin(None).validated_bin().unwrap(), DEFAULT_SCCACHE_BIN);
    }

    #[test]
    fn validated_bin_accepts_bare_command_name() {
        assert_eq!(with_bin(Some("sccache")).validated_bin().unwrap(), "sccache");
    }

    #[test]
    fn validated_bin_accepts_absolute_path() {
        let abs = if cfg!(windows) {
            r"C:\tools\sccache.exe"
        } else {
            "/usr/local/bin/sccache"
        };
        assert_eq!(with_bin(Some(abs)).validated_bin().unwrap(), abs);
    }

    #[test]
    fn validated_bin_rejects_relative_path_with_separator() {
        assert!(with_bin(Some("./evil")).validated_bin().is_err());
        assert!(with_bin(Some("a/b")).validated_bin().is_err());
        // A Windows-style separator is rejected on every platform.
        assert!(with_bin(Some(r"a\b")).validated_bin().is_err());
    }

    #[test]
    fn validated_bin_rejects_empty() {
        assert!(with_bin(Some("")).validated_bin().is_err());
    }
}
