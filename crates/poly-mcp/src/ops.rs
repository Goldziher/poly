//! Synchronous engine operations shared by every MCP tool.
//!
//! These functions are deliberately free of any `rmcp`/`tokio` types: they run
//! the same `poly-core` pipeline the CLI runs and serialize their outcome
//! with the **identical** JSON contract (`poly lint --format json` /
//! `poly fmt --format json`). The async tool handlers in [`crate::server`] call
//! them from a blocking task so the synchronous, rayon-driven engine never runs
//! on a tokio worker thread.

use std::path::{Path, PathBuf};

use poly_cache::ResultCache;
use poly_core::{Config, RunOptions, report};

/// Resolve the run configuration the way the CLI does: load an explicit file
/// when one is supplied, otherwise discover `poly.toml` from the working
/// directory (mirrors `poly_cli`'s `load_config`).
pub fn resolve_config(explicit: Option<&Path>) -> anyhow::Result<Config> {
    match explicit {
        Some(path) => Config::load_file(path),
        None => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            Config::load(&cwd)
        }
    }
}

/// Turn the request's path list into concrete paths, defaulting to the current
/// directory when the caller passes none (matching the CLI default).
fn resolve_paths(paths: &[String]) -> Vec<PathBuf> {
    if paths.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        paths.iter().map(PathBuf::from).collect()
    }
}

/// Lint `paths`. When `fix` is true, available autofixes are applied in place
/// before the remaining diagnostics are reported. Returns the same JSON the
/// CLI emits under `--format json`.
pub fn lint(paths: &[String], exclude: &[String], config: Option<&str>, fix: bool) -> anyhow::Result<String> {
    let explicit_config = config.is_some();
    let config = resolve_config(config.map(Path::new))?;
    let resolved = resolve_paths(paths);
    let opts = RunOptions {
        exclude: exclude.to_vec(),
        explicit_config,
        ..RunOptions::default()
    };
    let results = poly_core::lint(&resolved, &config, &opts, fix, false)?;
    Ok(report::report_lint_json(&results))
}

/// Format `paths`. When `write` is true, changed files are rewritten in place;
/// otherwise this is a dry run (`--check`). Returns the same JSON the CLI emits
/// under `--format json`.
pub fn format(paths: &[String], exclude: &[String], config: Option<&str>, write: bool) -> anyhow::Result<String> {
    let explicit_config = config.is_some();
    let config = resolve_config(config.map(Path::new))?;
    let resolved = resolve_paths(paths);
    let opts = RunOptions {
        exclude: exclude.to_vec(),
        explicit_config,
        ..RunOptions::default()
    };
    let results = poly_core::format(&resolved, &config, &opts, write, false)?;
    Ok(report::report_format_json(&results))
}

/// Report cache footprint (mirrors `poly cache stats`) as JSON.
pub fn cache_stats() -> anyhow::Result<String> {
    let cache = ResultCache::open_default(true)?;
    let stats = cache.stats()?;
    let per_namespace: Vec<serde_json::Value> = stats
        .per_namespace
        .iter()
        .map(|ns| {
            serde_json::json!({
                "namespace": ns.namespace.as_dir(),
                "entries": ns.entries,
                "bytes": ns.bytes,
            })
        })
        .collect();
    let value = serde_json::json!({
        "format_version": stats.format_version,
        "on_disk_version": stats.on_disk_version,
        "total_bytes": stats.total_bytes,
        "per_namespace": per_namespace,
    });
    Ok(serde_json::to_string_pretty(&value)?)
}

/// Remove every cached entry (mirrors `poly cache clean`) and report the freed
/// byte count as JSON.
pub fn cache_clean() -> anyhow::Result<String> {
    let cache = ResultCache::open_default(true)?;
    let freed = cache.clean()?;
    let value = serde_json::json!({ "freed_bytes": freed });
    Ok(serde_json::to_string_pretty(&value)?)
}
