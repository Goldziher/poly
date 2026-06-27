//! Parallel orchestration (rayon): discover files, route to backends, run with
//! content-hash caching, collect results. Defaults to all logical cores.

use std::path::PathBuf;
use std::sync::{Arc, Once};

use rayon::prelude::*;
use serde::Serialize;

use crate::cache::Cache;
use crate::config::{Config, Kind};
use crate::discover::{DiscoveredFile, discover};
use crate::engine::{Diagnostic, Edit, FormatOutput, SourceFile};
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

/// Maximum autofix passes per file: applying a fix can surface or resolve
/// others, so re-lint until stable, but cap to guarantee termination.
const MAX_FIX_PASSES: usize = 5;

/// Lint all discovered files under `paths`. Returns one [`LintResult`] per file
/// that still has at least one diagnostic. When `fix` is true, each file's
/// available autofixes are applied in place (re-linting until stable) before
/// the remaining, unfixable diagnostics are reported.
pub fn lint(
    paths: &[PathBuf],
    config: &Config,
    opts: &RunOptions,
    fix: bool,
) -> anyhow::Result<Vec<LintResult>> {
    configure_pool(opts.jobs);
    let cache = Cache::new(!opts.no_cache)?;
    let files = discover(paths);
    let mut results: Vec<LintResult> = files
        .par_iter()
        .filter_map(|f| lint_one(f, config, &cache, fix).ok())
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

fn lint_one(
    f: &DiscoveredFile,
    config: &Config,
    cache: &Cache,
    fix: bool,
) -> anyhow::Result<LintResult> {
    let original = std::fs::read_to_string(&f.path)?;
    let mut content = original.clone();
    let mut diagnostics = lint_content(f, config, cache, &content)?;

    if fix {
        for _ in 0..MAX_FIX_PASSES {
            let edits: Vec<&Edit> = diagnostics.iter().filter_map(|d| d.fix.as_ref()).collect();
            match apply_edits(&content, &edits) {
                Some(next) if next != content => {
                    content = next;
                    diagnostics = lint_content(f, config, cache, &content)?;
                }
                _ => break,
            }
        }
        if content != original {
            write_atomic(&f.path, &content)?;
        }
    }

    Ok(LintResult {
        path: f.path.clone(),
        diagnostics,
    })
}

/// Run every lint-capable engine for the file's language over `content`,
/// content-hash caching each engine's diagnostics.
fn lint_content(
    f: &DiscoveredFile,
    config: &Config,
    cache: &Cache,
    content: &str,
) -> anyhow::Result<Vec<Diagnostic>> {
    let src = SourceFile {
        path: f.path.clone(),
        language: f.language.clone(),
        content: Arc::from(content),
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
    Ok(all)
}

/// Apply non-overlapping byte-range autofixes to `content`. Edits are applied
/// right-to-left so earlier byte offsets stay valid; any edit that overlaps one
/// already applied, lands out of bounds, or falls on a non-char boundary is
/// skipped. Returns the rewritten text, or `None` if nothing was applied.
fn apply_edits(content: &str, edits: &[&Edit]) -> Option<String> {
    let mut sorted: Vec<&Edit> = edits.to_vec();
    sorted.sort_by_key(|e| std::cmp::Reverse(e.start_byte));
    let mut result = content.to_string();
    let mut prev_start = usize::MAX;
    let mut applied = false;
    for e in sorted {
        if e.start_byte > e.end_byte || e.end_byte > result.len() || e.end_byte > prev_start {
            continue;
        }
        if !result.is_char_boundary(e.start_byte) || !result.is_char_boundary(e.end_byte) {
            continue;
        }
        result.replace_range(e.start_byte..e.end_byte, &e.replacement);
        prev_start = e.start_byte;
        applied = true;
    }
    applied.then_some(result)
}

fn format_one(
    f: &DiscoveredFile,
    config: &Config,
    cache: &Cache,
    write: bool,
) -> anyhow::Result<FormatResult> {
    let original = std::fs::read_to_string(&f.path)?;
    // The file's bytes are shared across every format engine via `Arc<str>`:
    // each engine gets a refcount bump, not a fresh copy of the contents.
    let mut current: Arc<str> = Arc::from(original.as_str());
    // Construct the `SourceFile` once and re-point its content per engine
    // instead of rebuilding (and cloning the contents) on each iteration.
    let mut src = SourceFile {
        path: f.path.clone(),
        language: f.language.clone(),
        content: Arc::clone(&current),
    };
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
            current = Arc::from(text);
            continue;
        }
        src.content = Arc::clone(&current);
        let out: Arc<str> = match engine.format(&src, &ecfg)? {
            FormatOutput::Unchanged => Arc::clone(&current),
            FormatOutput::Formatted(s) => Arc::from(s),
        };
        let _ = cache.put(&key, out.as_bytes());
        current = out;
    }

    let changed = *current != *original;
    if changed && write {
        write_atomic(&f.path, &current)?;
    }
    Ok(FormatResult {
        path: f.path.clone(),
        changed,
        formatted: if changed {
            Some(current.to_string())
        } else {
            None
        },
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
