//! Shared hook-run helpers: resolve the result cache, sccache settings, and the
//! interactive-progress decision from a [`PolyConfig`].
//!
//! These are consumed both by this crate's [`crate::lint`] orchestration and by
//! the CLI's `poly hooks` runner, so they live here as the single source of
//! truth (the CLI re-imports them). They read only [`PolyConfig`] plus the
//! per-invocation flags — no CLI-only concerns — so a non-CLI caller (the MCP
//! server) can use them unchanged.

use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use poly_cache::ResultCache;
use poly_config::PolyConfig;

/// Whether to stream live per-hook progress: on when stderr is a terminal, so
/// interactive runs show which tool is running while captured logs stay quiet.
#[must_use]
pub fn show_progress() -> bool {
    std::io::stderr().is_terminal()
}

/// Open the tier-1 result cache for a hook run, honouring `[cache] enabled`,
/// the optional `[cache] dir` override, and the `--no-cache` flag.
///
/// Returns `None` when caching is disabled — the runner then neither reads nor
/// writes cache entries.
///
/// # Errors
///
/// Returns `Err` if the cache directory cannot be opened.
pub fn open_result_cache(config: &PolyConfig, root: &Path, no_cache: bool) -> Result<Option<ResultCache>> {
    let enabled = config.cache.enabled && !no_cache;
    let cache = match &config.cache.dir {
        Some(dir) => ResultCache::open(PathBuf::from(dir), enabled),
        None => ResultCache::open_from(root, enabled),
    }
    .context("failed to open the hook result cache")?;
    Ok(enabled.then_some(cache))
}

/// Resolve tier-2 sccache settings for a hook run from the `[cache.sccache]`
/// table, honouring the `--no-sccache` flag.
///
/// Returns `None` (sccache off) unless `[cache.sccache] enabled = true` and
/// `--no-sccache` was not given. The binary defaults to `"sccache"` when
/// `[cache.sccache] bin` is absent.
///
/// # Errors
///
/// Returns `Err` if the configured sccache binary name fails validation.
pub fn sccache_settings(config: &PolyConfig, no_sccache: bool) -> Result<Option<poly_hooks::SccacheSettings>> {
    let sccache = &config.cache.sccache;
    if !config.cache.enabled || !sccache.enabled || no_sccache {
        return Ok(None);
    }
    Ok(Some(poly_hooks::SccacheSettings {
        bin: sccache.validated_bin()?.to_string(),
        dir: sccache.dir.clone().map(PathBuf::from),
        max_size: sccache.max_size.clone(),
    }))
}
