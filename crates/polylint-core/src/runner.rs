//! Parallel orchestration (rayon): discover files, route to backends, run with
//! content-hash caching, collect results. Defaults to all logical cores.

use std::path::PathBuf;
use std::sync::Once;

use rayon::prelude::*;
use serde::Serialize;

use crate::cache::Cache;
use crate::config::{Config, Kind};
use crate::discover::{DiscoveredFile, discover};
use crate::engine::{Diagnostic, FormatOutput, SourceFile};
use crate::registry::engines_for;

/// Options controlling a lint/format run.
#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    /// Bypass the content-hash result cache.
    pub no_cache: bool,
    /// Number of worker threads; `None` => all logical cores.
    pub jobs: Option<usize>,
}

/// Per-file lint outcome.
#[derive(Debug, Clone, Serialize)]
pub struct LintResult {
    /// File that was linted.
    pub path: PathBuf,
    /// Diagnostics from all backends for this file.
    pub diagnostics: Vec<Diagnostic>,
}

/// Per-file format outcome.
#[derive(Debug, Clone, Serialize)]
pub struct FormatResult {
    /// File that was formatted.
    pub path: PathBuf,
    /// Whether formatting changed (or would change) the file.
    pub changed: bool,
    /// Formatted contents when changed (not serialized).
    #[serde(skip)]
    pub formatted: Option<String>,
}

/// Lint all discovered files under `paths`. Returns one [`LintResult`] per file
/// that produced at least one diagnostic.
pub fn lint(
    paths: &[PathBuf],
    config: &Config,
    opts: &RunOptions,
) -> anyhow::Result<Vec<LintResult>> {
    configure_pool(opts.jobs);
    let cache = Cache::new(!opts.no_cache)?;
    let files = discover(paths);
    let mut results: Vec<LintResult> = files
        .par_iter()
        .filter_map(|f| lint_one(f, config, &cache).ok())
        .filter(|r| !r.diagnostics.is_empty())
        .collect();
    results.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(results)
}

/// Format all discovered files under `paths`. When `write` is true, changed
/// files are rewritten atomically; otherwise this is a dry run (`--check`).
pub fn format(
    paths: &[PathBuf],
    config: &Config,
    opts: &RunOptions,
    write: bool,
) -> anyhow::Result<Vec<FormatResult>> {
    configure_pool(opts.jobs);
    let cache = Cache::new(!opts.no_cache)?;
    let files = discover(paths);
    let mut results: Vec<FormatResult> = files
        .par_iter()
        .filter_map(|f| format_one(f, config, &cache, write).ok())
        .collect();
    results.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(results)
}

fn lint_one(f: &DiscoveredFile, config: &Config, cache: &Cache) -> anyhow::Result<LintResult> {
    let content = std::fs::read_to_string(&f.path)?;
    let src = SourceFile {
        path: f.path.clone(),
        language: f.language.clone(),
        content,
    };
    let mut all = Vec::new();
    for engine in engines_for(&f.language) {
        if !engine.capabilities().lint {
            continue;
        }
        let ecfg = config.engine_config(&f.language, engine.name(), Kind::Lint);
        let key = Cache::key(
            &format!("lint:{}", engine.name()),
            engine.version(),
            &ecfg.options,
            &src.content,
        );
        if let Some(bytes) = cache.get(&key)
            && let Ok(diags) = serde_json::from_slice::<Vec<Diagnostic>>(&bytes)
        {
            all.extend(diags);
            continue;
        }
        let diags = engine.lint(&src, &ecfg)?;
        if let Ok(bytes) = serde_json::to_vec(&diags) {
            let _ = cache.put(&key, &bytes);
        }
        all.extend(diags);
    }
    Ok(LintResult {
        path: f.path.clone(),
        diagnostics: all,
    })
}

fn format_one(
    f: &DiscoveredFile,
    config: &Config,
    cache: &Cache,
    write: bool,
) -> anyhow::Result<FormatResult> {
    let original = std::fs::read_to_string(&f.path)?;
    let mut current = original.clone();
    for engine in engines_for(&f.language) {
        if !engine.capabilities().format {
            continue;
        }
        let ecfg = config.engine_config(&f.language, engine.name(), Kind::Format);
        let key = Cache::key(
            &format!("fmt:{}", engine.name()),
            engine.version(),
            &ecfg.options,
            &current,
        );
        if let Some(bytes) = cache.get(&key)
            && let Ok(text) = String::from_utf8(bytes)
        {
            current = text;
            continue;
        }
        let src = SourceFile {
            path: f.path.clone(),
            language: f.language.clone(),
            content: current.clone(),
        };
        let out = match engine.format(&src, &ecfg)? {
            FormatOutput::Unchanged => current.clone(),
            FormatOutput::Formatted(s) => s,
        };
        let _ = cache.put(&key, out.as_bytes());
        current = out;
    }

    let changed = current != original;
    if changed && write {
        write_atomic(&f.path, &current)?;
    }
    Ok(FormatResult {
        path: f.path.clone(),
        changed,
        formatted: if changed { Some(current) } else { None },
    })
}

fn write_atomic(path: &std::path::Path, contents: &str) -> anyhow::Result<()> {
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("polyfmt");
    let tmp = parent.join(format!(".{file_name}.{}.polyfmt.tmp", std::process::id()));
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn configure_pool(jobs: Option<usize>) {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let mut builder = rayon::ThreadPoolBuilder::new();
        if let Some(n) = jobs
            && n > 0
        {
            builder = builder.num_threads(n);
        }
        // Ignore error: the global pool may already be initialized by a caller.
        let _ = builder.build_global();
    });
}
