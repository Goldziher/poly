//! Parallel orchestration (rayon): discover files, route to backends, run with
//! content-hash caching, collect results. Defaults to all logical cores.

use std::path::PathBuf;
use std::sync::{Arc, Once};

use poly_cache::{Namespace, ResultCache};
use rayon::prelude::*;
use rustc_hash::FxHashSet;
use serde::Serialize;

use crate::config::{Config, Kind};
use crate::discover::{DiscoveredFile, DiscoveryReport, discover_reporting};
use crate::engine::{Diagnostic, Edit, FormatOutput, SourceFile};
use crate::filter::{
    PerFileIgnores, is_format_ignored, is_generated_lockfile, is_generated_source, match_bases, relative_for_match,
};
use crate::resolve::ConfigSet;

mod plan;

use plan::{EnginePlan, PlanMap, plan_by_config_language, prefetch_tier2_grammars};

/// Options controlling a lint/format run.
#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    /// Bypass the content-hash result cache.
    pub no_cache: bool,
    /// Number of worker threads; `None` => all logical cores.
    pub jobs: Option<usize>,
    /// Extra gitignore-style exclude globs supplied at call time (CLI `--exclude`
    /// / MCP `exclude`), merged with the config's `[discovery] exclude`.
    pub exclude: Vec<String>,
    /// Apply the exclude set to explicitly named files as well as to the walk.
    ///
    /// A hook is always handed explicit staged paths, so without this the
    /// repo's `[discovery] exclude` is silently inert exactly where it matters
    /// most. On for the hook path, off for a direct CLI invocation.
    pub force_exclude: bool,
    /// Apply `--fix` to machine-generated files too. Off by default: a fix there
    /// is reverted by the next generation run, and can silence the diagnostic
    /// that was the only evidence of a generator bug.
    pub fix_generated: bool,
    /// When `true`, the caller supplied an explicit `--config <path>`: use that
    /// single config for every file and skip hierarchical (nested `poly.toml`)
    /// resolution (ADR 0018). Default `false` — scan for nested configs.
    pub explicit_config: bool,
    /// Resolver for `extends` bases (ADR 0020). When set, nested `poly.toml`
    /// files resolve their `extends` list (local or pinned remote git bases)
    /// through this resolver during the cascade. `None` => local-only resolution
    /// via `LocalPathResolver` (the default; no remote fetch).
    pub config_resolver: Option<Arc<dyn poly_config::BaseConfigResolver>>,
}

/// Per-engine debug record for one file. Collected only when debug output is
/// requested (`--debug`); never built on the default hot path.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct EngineDebug {
    /// Backend that produced this record.
    pub engine: String,
    /// Wrapped tool/crate version (matches the cache-key component).
    pub version: String,
    /// Wall-clock time the engine spent on this file, in milliseconds. Zero for
    /// a cache hit (the engine did not run).
    pub duration_ms: f64,
    /// Whether the result came from the content-hash cache.
    pub cache_hit: bool,
}

/// Per-file debug data surfaced under `--debug`: cache hit/miss and timing for
/// each engine that ran. Populated only when debug collection is enabled.
#[derive(Debug, Clone, Serialize, Default)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct RunDebug {
    /// One entry per engine evaluated for the file.
    pub engines: Vec<EngineDebug>,
}

/// Per-file lint outcome.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct LintResult {
    /// File that was linted.
    pub path: PathBuf,
    /// Diagnostics from all backends for this file.
    pub diagnostics: Vec<Diagnostic>,
    /// Set when `--fix` was requested but withheld because the file announces
    /// itself as machine-generated. The diagnostics are still reported.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub fix_withheld_generated: bool,
    /// Debug data (cache hit/miss + timing), present only under `--debug`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debug: Option<RunDebug>,
}

/// Per-file format outcome.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct FormatResult {
    /// File that was formatted.
    pub path: PathBuf,
    /// Whether formatting changed (or would change) the file.
    pub changed: bool,
    /// Why no backend inspected this file, when none did.
    ///
    /// A file routed to a backend that declines it — YAML carrying Go/Helm
    /// template actions, a Jinja template rendering Go — was previously
    /// indistinguishable in the report from one that was checked and found
    /// clean. Carrying the reason lets the summary say so.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
    /// Formatted contents when changed (not serialized).
    #[serde(skip)]
    pub formatted: Option<String>,
    /// Debug data (cache hit/miss + timing), present only under `--debug`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debug: Option<RunDebug>,
}

/// A complete lint run: the per-file results plus the run-level accounting a
/// summary needs to qualify itself.
///
/// [`lint`] returns only the results, which cannot express "I checked nothing,
/// because everything was excluded" — the failure mode this type exists to fix.
#[derive(Debug, Clone)]
pub struct LintRun {
    /// Per-file results, one per file that still has at least one diagnostic.
    pub results: Vec<LintResult>,
    /// Files the per-file tier actually read and linted.
    pub checked: usize,
    /// What `[discovery] exclude` / `--exclude` pruned before any of that.
    pub discovery: DiscoveryReport,
}

/// A complete format run: the per-file results plus what discovery pruned.
#[derive(Debug, Clone)]
pub struct FormatRun {
    /// Per-file results, one per discovered file.
    pub results: Vec<FormatResult>,
    /// What `[discovery] exclude` / `--exclude` pruned before any of that.
    pub discovery: DiscoveryReport,
}

/// Maximum autofix passes per file: applying a fix can surface or resolve
/// others, so re-lint until stable, but cap to guarantee termination.
const MAX_FIX_PASSES: usize = 5;

/// Maximum format passes per file. A formatter is not guaranteed to be
/// idempotent — line-wrap/reflow can shift on a second run (observed with
/// clang-format on `.h`, csharpier on `.cs`, google-java-format on `.java`) —
/// so we re-run the whole engine chain until the content stops changing. Capped
/// to guarantee termination when a backend genuinely oscillates. Without this,
/// a single `poly fmt --fix` could leave a file that a subsequent `poly fmt
/// --check` still reports as unformatted.
const MAX_FORMAT_PASSES: usize = 5;

/// Lint all discovered files under `paths`. Returns one [`LintResult`] per file
/// that still has at least one diagnostic. When `fix` is true, each file's
/// available autofixes are applied in place (re-linting until stable) before
/// the remaining, unfixable diagnostics are reported.
pub fn lint(
    paths: &[PathBuf],
    config: &Config,
    opts: &RunOptions,
    fix: bool,
    collect_debug: bool,
) -> anyhow::Result<Vec<LintResult>> {
    Ok(lint_run(paths, config, opts, fix, collect_debug)?.results)
}

/// [`lint`], additionally returning the run-level accounting ([`LintRun`]) a
/// self-qualifying summary needs: how many files were actually linted, and what
/// the exclude set pruned before they were.
pub fn lint_run(
    paths: &[PathBuf],
    config: &Config,
    opts: &RunOptions,
    fix: bool,
    collect_debug: bool,
) -> anyhow::Result<LintRun> {
    configure_pool(opts.jobs);
    let cache = ResultCache::open_default(!opts.no_cache)?;
    let configs = build_config_set(paths, config, opts)?;
    let (files, discovery) = discover_reporting(paths, &configs, &opts.exclude, opts.force_exclude);
    let plans = plan_by_config_language(&files, &configs, Kind::Lint);
    prefetch_tier2_grammars(&plans);
    let ignores: Vec<PerFileIgnores> = configs
        .iter()
        .map(|c| PerFileIgnores::compile(&c.per_file_ignores))
        .collect();
    let bases = match_bases(paths);
    // One relaxed increment per file, so the summary can state how many files
    // were genuinely linted rather than how many were handed to the pipeline.
    // Negligible next to reading and parsing the file it counts.
    let checked = std::sync::atomic::AtomicUsize::new(0);
    let mut results: Vec<LintResult> = files
        .par_iter()
        .filter_map(|f| {
            match lint_one(
                f,
                &plans,
                &cache,
                fix,
                opts.fix_generated,
                collect_debug,
                &configs,
                &ignores,
                &bases,
            ) {
                Ok(result) => {
                    checked.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    Some(result)
                }
                Err(error) => {
                    tracing::warn!(path = %f.path.display(), "lint failed: {error:#}");
                    None
                }
            }
        })
        .filter(|r| !r.diagnostics.is_empty())
        .collect();
    results.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(LintRun {
        results,
        checked: checked.into_inner(),
        discovery,
    })
}

/// Build the run's [`ConfigSet`]: a single explicit config (`--config`) bypasses
/// hierarchical resolution; otherwise `config` is the root and the walked paths
/// are scanned for nested `poly.toml` files (ADR 0018).
fn build_config_set(paths: &[PathBuf], config: &Config, opts: &RunOptions) -> anyhow::Result<ConfigSet> {
    if opts.explicit_config {
        Ok(ConfigSet::single(config.clone()))
    } else if let Some(resolver) = &opts.config_resolver {
        ConfigSet::build_with(paths, config.clone(), resolver.as_ref())
    } else {
        ConfigSet::build(paths, config.clone())
    }
}

/// Format all discovered files under `paths`. When `write` is true, changed
/// files are rewritten atomically; otherwise this is a dry run (`--check`).
pub fn format(
    paths: &[PathBuf],
    config: &Config,
    opts: &RunOptions,
    write: bool,
    collect_debug: bool,
) -> anyhow::Result<Vec<FormatResult>> {
    Ok(format_run(paths, config, opts, write, collect_debug)?.results)
}

/// [`format()`], additionally returning the run-level accounting ([`FormatRun`])
/// a self-qualifying summary needs: what the exclude set pruned before the
/// discovered files were formatted.
pub fn format_run(
    paths: &[PathBuf],
    config: &Config,
    opts: &RunOptions,
    write: bool,
    collect_debug: bool,
) -> anyhow::Result<FormatRun> {
    configure_pool(opts.jobs);
    let cache = ResultCache::open_default(!opts.no_cache)?;
    let explicit: FxHashSet<&std::path::Path> = paths.iter().map(PathBuf::as_path).collect();
    let configs = build_config_set(paths, config, opts)?;
    let (discovered, discovery) = discover_reporting(paths, &configs, &opts.exclude, opts.force_exclude);
    let files: Vec<DiscoveredFile> = discovered
        .into_iter()
        .filter(|f| explicit.contains(f.path.as_path()) || !is_generated_lockfile(&f.path))
        .collect();
    let plans = plan_by_config_language(&files, &configs, Kind::Format);
    prefetch_tier2_grammars(&plans);
    let mut results: Vec<FormatResult> = files
        .par_iter()
        .filter_map(|f| match format_one(f, &plans, &cache, write, collect_debug) {
            Ok(result) => Some(result),
            Err(error) => {
                tracing::warn!(path = %f.path.display(), "format failed: {error:#}");
                None
            }
        })
        .collect();
    results.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(FormatRun { results, discovery })
}

#[allow(clippy::too_many_arguments)]
fn lint_one(
    f: &DiscoveredFile,
    plans: &PlanMap,
    cache: &ResultCache,
    fix: bool,
    fix_generated: bool,
    collect_debug: bool,
    configs: &ConfigSet,
    ignores: &[PerFileIgnores],
    bases: &[PathBuf],
) -> anyhow::Result<LintResult> {
    let original = std::fs::read_to_string(&f.path)?;
    let this_ignores = &ignores[f.config_id];
    let rel =
        (!this_ignores.is_empty()).then(|| relative_for_match(&f.path, &configs.ignore_bases(f.config_id, bases)));
    let suppress = |diagnostics: &mut Vec<Diagnostic>| {
        if let Some(rel) = &rel {
            this_ignores.apply(rel, diagnostics);
        }
    };

    let (mut diagnostics, mut debug) = lint_content(f, plans, cache, &original, collect_debug)?;
    suppress(&mut diagnostics);

    // Report on generated files but never rewrite them. A fix there is churn the
    // next generation run reverts, and it can silence the diagnostic that was the
    // only evidence of a generator bug — `--fix-generated` opts back in.
    let generated = fix && !fix_generated && is_generated_source(&original);
    if generated {
        tracing::debug!(path = %f.path.display(), "skipping --fix on generated file");
    }

    if fix && !generated {
        let mut content = original.clone();
        for _ in 0..MAX_FIX_PASSES {
            let edit_groups: Vec<&[Edit]> = diagnostics
                .iter()
                .filter(|d| !d.fix.is_empty())
                .map(|d| d.fix.as_slice())
                .collect();
            match apply_edits(&content, &edit_groups) {
                Some(next) if next != content => {
                    content = next;
                    let (next_diags, next_debug) = lint_content(f, plans, cache, &content, collect_debug)?;
                    diagnostics = next_diags;
                    suppress(&mut diagnostics);
                    debug = next_debug;
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
        fix_withheld_generated: generated,
        debug,
    })
}

/// Run every lint-capable engine for the file's language over `content`,
/// content-hash caching each engine's diagnostics. When `collect_debug` is set,
/// also returns per-engine cache hit/miss + timing; otherwise the second tuple
/// element is `None` and no timing instrumentation runs.
fn lint_content(
    f: &DiscoveredFile,
    plans: &PlanMap,
    cache: &ResultCache,
    content: &str,
    collect_debug: bool,
) -> anyhow::Result<(Vec<Diagnostic>, Option<RunDebug>)> {
    let src = SourceFile {
        path: f.path.clone(),
        language: f.language.clone(),
        content: Arc::from(content),
    };
    let digest = ResultCache::single_file_digest_with_path(&f.path.to_string_lossy(), content);
    let mut all = Vec::new();
    let mut debug = collect_debug.then(RunDebug::default);
    let engine_plans = plans
        .get(&(f.config_id, f.language.clone()))
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    for plan in engine_plans {
        let key = ResultCache::key_with_args(
            Namespace::Lint,
            plan.engine.name(),
            plan.engine.version(),
            &plan.serialized_args,
            &digest,
        );
        if let Some(bytes) = cache.get(Namespace::Lint, &key)
            && let Ok(mut diags) = serde_json::from_slice::<Vec<Diagnostic>>(&bytes)
        {
            push_engine_debug(debug.as_mut(), plan, None);
            if !plan.severity_remap.is_empty() {
                plan.severity_remap.apply(&mut diags);
            }
            all.extend(diags);
            continue;
        }
        let started = collect_debug.then(std::time::Instant::now);
        let mut diags = plan.engine.lint(&src, &plan.config)?;
        push_engine_debug(debug.as_mut(), plan, started);
        if let Ok(bytes) = serde_json::to_vec(&diags)
            && let Err(error) = cache.put(Namespace::Lint, &key, &bytes)
        {
            tracing::warn!(
                engine = plan.engine.name(),
                "failed to store lint cache entry: {error:#}"
            );
        }
        if !plan.severity_remap.is_empty() {
            plan.severity_remap.apply(&mut diags);
        }
        all.extend(diags);
    }
    Ok((all, debug))
}

/// Append one [`EngineDebug`] record when debug collection is active. `started`
/// is `Some` for an engine that actually ran (timing it) and `None` for a cache
/// hit (`duration_ms` = 0, `cache_hit` = true).
fn push_engine_debug(debug: Option<&mut RunDebug>, plan: &EnginePlan, started: Option<std::time::Instant>) {
    if let Some(debug) = debug {
        let (duration_ms, cache_hit) = match started {
            Some(start) => (start.elapsed().as_secs_f64() * 1000.0, false),
            None => (0.0, true),
        };
        debug.engines.push(EngineDebug {
            engine: plan.engine.name().to_owned(),
            version: plan.engine.version().to_owned(),
            duration_ms,
            cache_hit,
        });
    }
}

/// Apply autofix edit groups to `content`, one group per diagnostic.
///
/// Each group is the full `fix` vec of one [`Diagnostic`] and is applied
/// **atomically**: all of its edits apply, or none do.
///
/// Selection rules (right-to-left):
/// 1. Any group whose own edits overlap each other internally is discarded
///    (prevents corrupted output from a malformed backend fix).
/// 2. Groups are attempted rightmost-first.  If any edit in a group would
///    reach into bytes already committed by a previously-applied group, the
///    entire group is skipped; the convergence loop in [`lint_one`] will retry
///    it on the next pass once those diagnostics have been re-evaluated.
///
/// Returns the rewritten text, or `None` if no edit was applied.
fn apply_edits(content: &str, edit_groups: &[&[Edit]]) -> Option<String> {
    let mut valid: Vec<&[Edit]> = edit_groups
        .iter()
        .copied()
        .filter(|g| !g.is_empty() && !has_internal_overlap(g))
        .collect();
    valid.sort_by_key(|g| std::cmp::Reverse(g.iter().map(|e| e.end_byte).max().unwrap_or(0)));

    let mut result = content.to_string();
    let mut prev_start = usize::MAX;
    let mut applied = false;

    'groups: for group in &valid {
        for e in *group {
            if e.start_byte > e.end_byte || e.end_byte > result.len() || e.end_byte > prev_start {
                continue 'groups;
            }
            if !result.is_char_boundary(e.start_byte) || !result.is_char_boundary(e.end_byte) {
                continue 'groups;
            }
        }

        if let [e] = *group {
            result.replace_range(e.start_byte..e.end_byte, &e.replacement);
        } else {
            let mut ordered: Vec<&Edit> = group.iter().collect();
            ordered.sort_by_key(|e| std::cmp::Reverse(e.start_byte));
            for e in &ordered {
                result.replace_range(e.start_byte..e.end_byte, &e.replacement);
            }
        }

        prev_start = group.iter().map(|e| e.start_byte).min().unwrap_or(prev_start);
        applied = true;
    }

    applied.then_some(result)
}

/// Returns `true` when any two edits in `group` have overlapping byte ranges.
///
/// O(n²) — acceptable because fix groups are tiny (1–4 edits in practice).
fn has_internal_overlap(group: &[Edit]) -> bool {
    for (i, a) in group.iter().enumerate() {
        for b in group.iter().skip(i + 1) {
            let intersects = a.start_byte < b.end_byte && b.start_byte < a.end_byte;
            let same_point_insert =
                a.start_byte == a.end_byte && b.start_byte == b.end_byte && a.start_byte == b.start_byte;
            if intersects || same_point_insert {
                return true;
            }
        }
    }
    false
}

fn format_one(
    f: &DiscoveredFile,
    plans: &PlanMap,
    cache: &ResultCache,
    write: bool,
    collect_debug: bool,
) -> anyhow::Result<FormatResult> {
    let original = std::fs::read_to_string(&f.path)?;
    if is_format_ignored(&original, &f.language) {
        return Ok(FormatResult {
            path: f.path.clone(),
            changed: false,
            formatted: None,
            skipped: None,
            debug: None,
        });
    }
    let mut debug = collect_debug.then(RunDebug::default);
    let mut src = SourceFile {
        path: f.path.clone(),
        language: f.language.clone(),
        content: Arc::from(original.as_str()),
    };
    let engine_plans = plans
        .get(&(f.config_id, f.language.clone()))
        .map(Vec::as_slice)
        .unwrap_or(&[]);

    // When every engine routed to this file declines it, nothing inspected the
    // content — report that rather than letting it read as "checked and clean".
    let skipped = skip_reason_for(engine_plans, &src);

    // Run every format engine once over `input`, returning the chained output.
    // Debug records are collected on the first pass only (`record_debug`) so the
    // convergence loop below does not inflate per-engine timing counts.
    let run_pass = |input: &Arc<str>, record_debug: bool| -> anyhow::Result<Arc<str>> {
        let mut current = Arc::clone(input);
        for plan in engine_plans {
            let digest = ResultCache::single_file_digest(&current);
            let key = ResultCache::key_with_args(
                Namespace::Fmt,
                plan.engine.name(),
                plan.engine.version(),
                &plan.serialized_args,
                &digest,
            );
            if let Some(bytes) = cache.get(Namespace::Fmt, &key)
                && let Ok(text) = String::from_utf8(bytes)
            {
                if record_debug {
                    push_engine_debug(debug.as_mut(), plan, None);
                }
                current = Arc::from(text);
                continue;
            }
            src.content = Arc::clone(&current);
            let started = collect_debug.then(std::time::Instant::now);
            let out: Arc<str> = match plan.engine.format(&src, &plan.config)? {
                FormatOutput::Unchanged => Arc::clone(&current),
                FormatOutput::Formatted(s) => Arc::from(s),
            };
            if record_debug {
                push_engine_debug(debug.as_mut(), plan, started);
            }
            if let Err(error) = cache.put(Namespace::Fmt, &key, out.as_bytes()) {
                tracing::warn!(
                    engine = plan.engine.name(),
                    "failed to store fmt cache entry: {error:#}"
                );
            }
            current = out;
        }
        Ok(current)
    };

    let current = format_to_fixed_point(Arc::from(original.as_str()), run_pass)?;

    let changed = *current != *original;
    if changed && write {
        write_atomic(&f.path, &current)?;
    }
    Ok(FormatResult {
        path: f.path.clone(),
        changed,
        formatted: if changed { Some(current.to_string()) } else { None },
        skipped,
        debug,
    })
}

/// The reason no backend inspected `src`, when every engine routed to it
/// declines.
///
/// Returns `None` when at least one engine will actually look at the file, so a
/// file served by both a declining and a willing backend is not reported as
/// skipped. An empty plan means no backend covers the language at all, which is
/// ordinary coverage rather than a decline, and is likewise not reported here.
fn skip_reason_for(plans: &[EnginePlan], src: &SourceFile) -> Option<String> {
    if plans.is_empty() {
        return None;
    }
    let mut reason = None;
    for plan in plans {
        // `?` short-circuits the moment any engine is willing to look at the
        // file: one willing backend means the file was not skipped.
        let why = plan.engine.skip_reason(src)?;
        reason.get_or_insert_with(|| why.to_owned());
    }
    reason
}

/// Drive `run_pass` (one full format-engine chain over the content) to a fixed
/// point: re-run it until the output stops changing, bounded by
/// [`MAX_FORMAT_PASSES`] so a genuinely oscillating backend still terminates.
///
/// The first pass receives `record_debug == true`; every retry receives `false`
/// so per-engine debug counts reflect a single logical run. A run that is already
/// stable costs exactly one pass; each additional pass only happens when the
/// previous one changed the content (so `poly fmt --fix` is a fixed point and a
/// following `poly fmt --check` is clean).
fn format_to_fixed_point<F>(initial: Arc<str>, mut run_pass: F) -> anyhow::Result<Arc<str>>
where
    F: FnMut(&Arc<str>, bool) -> anyhow::Result<Arc<str>>,
{
    let mut current = initial;
    for pass in 0..MAX_FORMAT_PASSES {
        let next = run_pass(&current, pass == 0)?;
        let stable = *next == *current;
        current = next;
        if stable {
            break;
        }
    }
    Ok(current)
}

fn write_atomic(path: &std::path::Path, contents: &str) -> anyhow::Result<()> {
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("poly");
    let tmp = parent.join(format!(".{file_name}.{}.poly.tmp", std::process::id()));
    let original_permissions = std::fs::metadata(path).ok().map(|m| m.permissions());
    std::fs::write(&tmp, contents)?;
    // ~keep The rename replaces the original inode with a freshly created temp file, whose mode is
    // `0666 & !umask` and has no relationship to the file being formatted. Without this, formatting
    // an executable script silently clears its exec bit.
    if let Some(permissions) = original_permissions {
        std::fs::set_permissions(&tmp, permissions)?;
    }
    if let Err(error) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(error.into());
    }
    Ok(())
}

/// Stack size for rayon worker threads, in bytes (16 MiB).
///
/// rayon workers default to Rust's spawned-thread stack of 2 MiB, but the
/// per-file engines run recursive-descent parsers/formatters (oxc, mago,
/// markup_fmt, the tree-sitter reindent) whose recursion depth tracks source
/// nesting. On real-world files that 2 MiB is not enough and a worker overflows
/// its stack — an uncatchable abort that takes down the whole run. The process
/// main thread already gets 8 MiB (which is why single-file, inline runs never
/// crashed); we give workers a generous 16 MiB so a deeply nested file degrades
/// to a normal result instead of aborting.
const WORKER_STACK_SIZE: usize = 16 * 1024 * 1024;

fn configure_pool(jobs: Option<usize>) {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let mut builder = rayon::ThreadPoolBuilder::new().stack_size(WORKER_STACK_SIZE);
        if let Some(n) = jobs
            && n > 0
        {
            builder = builder.num_threads(n);
        }
        let _ = builder.build_global();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(start: usize, end: usize, rep: &str) -> Edit {
        Edit {
            start_byte: start,
            end_byte: end,
            replacement: rep.to_owned(),
        }
    }

    /// A pass that is already at its fixed point runs exactly once — no wasted
    /// confirmation pass, and the content is returned unchanged.
    #[test]
    fn format_to_fixed_point_stable_input_runs_once() {
        let calls = std::cell::Cell::new(0);
        let result = format_to_fixed_point(Arc::from("stable"), |content, _record| {
            calls.set(calls.get() + 1);
            Ok(Arc::clone(content))
        })
        .unwrap();
        assert_eq!(&*result, "stable");
        assert_eq!(calls.get(), 1, "an already-stable input needs a single pass");
    }

    /// A non-idempotent pass (strips one trailing '!' per run) converges within
    /// the bound, and the driver stops as soon as a pass makes no change — so the
    /// result is a genuine fixed point, mirroring the `fmt --fix` then `--check`
    /// invariant.
    #[test]
    fn format_to_fixed_point_converges_on_non_idempotent_pass() {
        let calls = std::cell::Cell::new(0);
        let result = format_to_fixed_point(Arc::from("a!!!"), |content, _record| {
            calls.set(calls.get() + 1);
            let stripped = content.strip_suffix('!').unwrap_or(content);
            Ok(Arc::from(stripped))
        })
        .unwrap();
        assert_eq!(&*result, "a", "trailing markers fully removed");
        // "a!!!"->"a!!"->"a!"->"a" is 3 changing passes plus 1 no-op that proves
        // stability = 4 calls. ~keep
        assert_eq!(calls.get(), 4);
    }

    /// Only the first pass records debug so per-engine timing counts are not
    /// inflated by the convergence retries.
    #[test]
    fn format_to_fixed_point_records_debug_on_first_pass_only() {
        let recorded: std::cell::RefCell<Vec<bool>> = std::cell::RefCell::new(Vec::new());
        format_to_fixed_point(Arc::from("a!!"), |content, record| {
            recorded.borrow_mut().push(record);
            Ok(Arc::from(content.strip_suffix('!').unwrap_or(content)))
        })
        .unwrap();
        assert_eq!(
            recorded.into_inner(),
            vec![true, false, false],
            "debug is recorded on the first pass, never on a retry"
        );
    }

    /// A backend that never stabilizes is bounded to `MAX_FORMAT_PASSES` runs
    /// rather than looping forever.
    #[test]
    fn format_to_fixed_point_bounds_a_never_stable_pass() {
        let calls = std::cell::Cell::new(0);
        let result = format_to_fixed_point(Arc::from("x"), |content, _record| {
            calls.set(calls.get() + 1);
            Ok(Arc::from(format!("{content}y")))
        })
        .unwrap();
        assert_eq!(calls.get(), MAX_FORMAT_PASSES, "oscillation is capped, not infinite");
        assert_eq!(&*result, "xyyyyy", "returns the last bounded pass output");
    }

    /// Two diagnostics, each with two non-overlapping edits; all four apply.
    #[test]
    fn multi_edit_two_groups_apply_atomically() {
        let content = "hello world foo";
        let group_a = vec![edit(6, 11, "earth"), edit(12, 15, "bar")];
        let group_b = vec![edit(0, 5, "hey")];

        let result = apply_edits(content, &[group_a.as_slice(), group_b.as_slice()]).expect("should produce output");
        assert_eq!(result, "hey earth bar");
    }

    /// A diagnostic whose edits overlap each other is skipped entirely.
    #[test]
    fn overlapping_edits_within_group_are_skipped() {
        let content = "abcdefgh";
        let bad_group = vec![edit(2, 6, "X"), edit(4, 8, "Y")];

        let result = apply_edits(content, &[bad_group.as_slice()]);
        assert!(result.is_none(), "overlapping group must produce no output");
    }

    /// When two groups from different diagnostics conflict, the leftward group
    /// is deferred (not corrupted).
    #[test]
    fn cross_group_conflict_defers_leftward_group() {
        let content = "abcde";
        let group_a = vec![edit(3, 5, "XX")];
        let group_b = vec![edit(2, 4, "YY")];

        let result = apply_edits(content, &[group_a.as_slice(), group_b.as_slice()])
            .expect("should produce output from group A");
        assert_eq!(result, "abcXX");
    }

    #[test]
    fn non_overlapping_edits_pass_internal_check() {
        let group = vec![edit(0, 5, "a"), edit(5, 10, "b")];
        assert!(!has_internal_overlap(&group));
    }

    #[test]
    fn adjacent_edits_are_not_overlapping() {
        let group = vec![edit(0, 5, "a"), edit(5, 10, "b")];
        assert!(!has_internal_overlap(&group));
    }

    #[test]
    fn touching_edits_with_overlap_detected() {
        let group = vec![edit(0, 6, "a"), edit(4, 10, "b")];
        assert!(has_internal_overlap(&group));
    }

    /// Recurse `depth` frames, each pinning ~8 KiB of stack, returning the
    /// accumulated depth. `black_box` keeps the per-frame buffer from being
    /// optimised away, so the stack actually grows.
    fn recurse_pinning_stack(depth: usize) -> usize {
        let mut frame = [0u8; 8 * 1024];
        frame[0] = (depth & 0xff) as u8;
        std::hint::black_box(&frame);
        if depth == 0 {
            frame[0] as usize
        } else {
            recurse_pinning_stack(depth - 1).wrapping_add(1)
        }
    }

    /// A worker thread sized at [`WORKER_STACK_SIZE`] must accommodate recursion
    /// far deeper than the 2 MiB default rayon stack — the regression that made
    /// per-file engines abort the whole run on nested real-world files
    /// (spikard corpus). ~640 frames × 8 KiB ≈ 5 MiB of pinned stack overflows
    /// the old 2 MiB default but fits comfortably in 16 MiB.
    #[test]
    fn worker_stack_accommodates_deep_recursion() {
        const FRAMES: usize = 640;
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .stack_size(WORKER_STACK_SIZE)
            .build()
            .expect("build local pool");
        let result = pool.install(|| recurse_pinning_stack(FRAMES));
        assert_eq!(result, FRAMES);
    }
}
