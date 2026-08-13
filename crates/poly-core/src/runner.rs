//! Parallel orchestration (rayon): discover files, route to backends, run with
//! content-hash caching, collect results. Defaults to all logical cores.

use std::path::PathBuf;
use std::sync::{Arc, Once};

use crate::config::{Config, Kind};
use crate::discover::{DiscoveredFile, discover_reporting};
use crate::engine::{Diagnostic, Edit, FormatOutput, SourceFile};
use crate::filter::{
    PerFileIgnores, is_format_ignored, is_generated_lockfile, is_generated_source, is_hash_stamped_source, match_bases,
    relative_for_match,
};
use crate::language::Language;
use crate::resolve::ConfigSet;
use poly_cache::{Namespace, ResultCache};
use rayon::prelude::*;
use rustc_hash::FxHashSet;

mod edits;
mod plan;
mod skips;
mod types;

use edits::apply_edits;
use plan::{EnginePlan, PlanMap, plan_by_config_language, prefetch_tier2_grammars, provides_language_lint};
use skips::unmatched_explicit_paths;
pub use skips::{NO_ENGINE_SKIP, NO_LINT_RULES_SKIP_PREFIX, SkippedFile};
// Re-exported so `poly_core::runner::LintResult` keeps naming the same type it
// always has: the split below is a file boundary, not an API one.
pub use types::{
    EngineDebug, FormatError, FormatResult, FormatRun, LintError, LintResult, LintRun, RunDebug, RunOptions,
};

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

/// Reason reported when `poly fmt` leaves a machine-generated file alone.
const GENERATED_SKIP: &str = "hash-stamped generated file (pass --fix-generated to format)";

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
    // An engine error is carried, not swallowed: dropping the file here is what
    // let `poly lint` report success on a file it had failed to process.
    let (oks, errs): (Vec<_>, Vec<_>) = files
        .par_iter()
        .map(|f| {
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
                &opts.externally_linted_languages,
            ) {
                Ok(result) => {
                    // A file no backend has rules for is not part of the linted
                    // count: it was routed and read, but nothing in the run knew
                    // its language, so counting it is the claim this fix removes.
                    if result.skipped.is_none() {
                        checked.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    Ok(result)
                }
                Err(error) => Err(LintError {
                    path: f.path.clone(),
                    message: format!("{error:#}"),
                }),
            }
        })
        .partition(Result::is_ok);
    let mut linted: Vec<LintResult> = oks.into_iter().map(Result::unwrap).collect();
    linted.sort_by(|a, b| a.path.cmp(&b.path));
    // Taken before the filter below, which keeps only files with something to
    // report: a skipped file has nothing to report and would be dropped, which
    // is how it became invisible in the first place.
    let mut skipped: Vec<SkippedFile> = linted
        .iter()
        .filter_map(|result| {
            result.skipped.as_ref().map(|reason| SkippedFile {
                path: result.path.clone(),
                reason: reason.clone(),
            })
        })
        .collect();
    // A fully fixed file has no diagnostics left, but dropping it here is
    // what made `--fix` silent about the files it rewrote: keep it so the
    // summary — and the JSON payload — can report the fixes.
    let results: Vec<LintResult> = linted
        .into_iter()
        .filter(|r| !r.diagnostics.is_empty() || r.fixed > 0)
        .collect();
    let mut errors: Vec<LintError> = errs.into_iter().map(Result::unwrap_err).collect();
    errors.sort_by(|a, b| a.path.cmp(&b.path));
    for error in &errors {
        tracing::warn!(path = %error.path.display(), "lint failed: {}", error.message);
    }
    skipped.extend(
        unmatched_explicit_paths(paths, &files, &plans, &configs, &opts.exclude, opts.force_exclude)
            .iter()
            .map(|path| SkippedFile::no_engine(path)),
    );
    Ok(LintRun {
        results,
        errors,
        checked: checked.into_inner(),
        skipped,
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
    // An engine error is carried, not swallowed: dropping the file here is what
    // let `poly fmt --check` report success on a file it could not parse.
    let (oks, errs): (Vec<_>, Vec<_>) = files
        .par_iter()
        .map(|f| {
            format_one(f, &plans, &cache, write, opts.fix_generated, collect_debug).map_err(|error| FormatError {
                path: f.path.clone(),
                message: format!("{error:#}"),
            })
        })
        .partition(Result::is_ok);
    let mut results: Vec<FormatResult> = oks.into_iter().map(Result::unwrap).collect();
    let mut errors: Vec<FormatError> = errs.into_iter().map(Result::unwrap_err).collect();
    results.sort_by(|a, b| a.path.cmp(&b.path));
    errors.sort_by(|a, b| a.path.cmp(&b.path));
    for error in &errors {
        tracing::warn!(path = %error.path.display(), "format failed: {}", error.message);
    }
    // Declined files first (they are already sorted by path), then the paths the
    // caller named that no engine covers — one list, so a strict-mode check and
    // the summary do not have to know which kind they are looking at.
    let mut skipped: Vec<SkippedFile> = results
        .iter()
        .filter_map(|result| {
            result.skipped.as_ref().map(|reason| SkippedFile {
                path: result.path.clone(),
                reason: reason.clone(),
            })
        })
        .collect();
    skipped.extend(
        unmatched_explicit_paths(paths, &files, &plans, &configs, &opts.exclude, opts.force_exclude)
            .iter()
            .map(|path| SkippedFile::no_engine(path)),
    );
    Ok(FormatRun {
        results,
        errors,
        skipped,
        discovery,
    })
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
    externally_linted: &[Language],
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
    // Resolved once per file rather than inside `lint_content`, which the fix
    // loop below calls up to `MAX_FIX_PASSES` times — each of those was a hash
    // lookup keyed on a freshly cloned `Language`.
    let engine_plans = plans
        .get(&(f.config_id, f.language.clone()))
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    // A language nothing holds rules for is not a clean file: the cross-cutting
    // backends below still run and can still report findings, but no rule in
    // this run knows the language, so the file must not be counted as linted.
    //
    // `externally_linted` is the caller's declaration that another phase of the
    // *same* run does lint the language (`cargo clippy` over Rust). A linear
    // scan is the right shape here: the list holds one entry per whole-project
    // tool, so it is a handful of comparisons against a slice already in cache —
    // cheaper than the hash it would take to avoid them.
    let skipped = (!provides_language_lint(engine_plans) && !externally_linted.contains(&f.language))
        .then(|| SkippedFile::no_lint_rules_reason(&f.language));

    let (mut diagnostics, mut debug) = lint_content(f, engine_plans, cache, &original, collect_debug)?;
    suppress(&mut diagnostics);

    // Report on generated files but never rewrite them. A fix there is churn the
    // next generation run reverts, and it can silence the diagnostic that was the
    // only evidence of a generator bug — `--fix-generated` opts back in.
    let generated = fix && !fix_generated && is_generated_source(&original);
    if generated {
        tracing::debug!(path = %f.path.display(), "skipping --fix on generated file");
    }

    // Counted across passes rather than derived from the drop in diagnostic
    // count: applying one fix can surface another, so a before/after delta would
    // understate (or invert) what the run actually did. This is the number of
    // diagnostics whose edits were committed to the file.
    let mut fixed = 0usize;
    if fix && !generated {
        let mut content = original.clone();
        for _ in 0..MAX_FIX_PASSES {
            let edit_groups: Vec<&[Edit]> = diagnostics
                .iter()
                .filter(|d| !d.fix.is_empty())
                .map(|d| d.fix.as_slice())
                .collect();
            match apply_edits(&content, &edit_groups) {
                Some((next, applied)) if next != content => {
                    content = next;
                    fixed += applied;
                    let (next_diags, next_debug) = lint_content(f, engine_plans, cache, &content, collect_debug)?;
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
        fixed,
        skipped,
        error: None,
        debug,
    })
}

/// Run every lint-capable engine for the file's language over `content`,
/// content-hash caching each engine's diagnostics. When `collect_debug` is set,
/// also returns per-engine cache hit/miss + timing; otherwise the second tuple
/// element is `None` and no timing instrumentation runs.
fn lint_content(
    f: &DiscoveredFile,
    engine_plans: &[EnginePlan],
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

fn format_one(
    f: &DiscoveredFile,
    plans: &PlanMap,
    cache: &ResultCache,
    write: bool,
    fix_generated: bool,
    collect_debug: bool,
) -> anyhow::Result<FormatResult> {
    let original = std::fs::read_to_string(&f.path)?;
    if is_format_ignored(&original, &f.language) {
        return Ok(FormatResult {
            path: f.path.clone(),
            changed: false,
            formatted: None,
            skipped: None,
            error: None,
            debug: None,
        });
    }
    // Skip only when the header stamps a **content hash** over the body:
    // reformatting invalidates it, so a verify step reports drift on a file no
    // human touched and the remedy is a regen that discards the formatting —
    // a loop, and one reporter had 110 of 123 files in it.
    //
    // Deliberately narrower than `is_generated_source`, which `lint --fix` uses.
    // Skipping here removes the file from the format gate entirely, so a bare
    // "DO NOT EDIT" banner must not trigger it: a generator that stamps a
    // hand-written file would otherwise drop it out of enforcement silently.
    // Formatting a banner-only file is harmless; not checking it is not.
    //
    // Skipped rather than reported as drift: poly will not fix these, so
    // flagging them under `--check` would leave a gate that can never go green.
    if !fix_generated && is_hash_stamped_source(&original) {
        return Ok(FormatResult {
            path: f.path.clone(),
            changed: false,
            formatted: None,
            skipped: Some(GENERATED_SKIP.to_owned()),
            error: None,
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
    // Asking is the whole decision: a backend that declined here would only
    // re-derive the same answer inside `format` — for the content-scanning ones
    // (`rumdl`, `yaml`, `markup_fmt`) by scanning the file a second time — and
    // then return `Unchanged`, so the chain below is skipped outright. That
    // equivalence is the [`Engine::skip_reason`] contract: a backend that
    // declines a file must not reformat it.
    let skipped = skip_reason_for(engine_plans, &src);
    if skipped.is_some() {
        return Ok(FormatResult {
            path: f.path.clone(),
            changed: false,
            formatted: None,
            skipped,
            error: None,
            // No engine ran, so there is nothing to time: the debug block
            // reports engines that executed, as it does for the skips above.
            debug: None,
        });
    }

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
        error: None,
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
///
/// A `Some` answer stands in for running the chain: every backend declined, and
/// a backend that declines a file must not reformat it, so the format pass is
/// skipped rather than asking each of them the same question a second time.
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
