//! Parallel orchestration (rayon): discover files, route to backends, run with
//! content-hash caching, collect results. Defaults to all logical cores.

use std::path::PathBuf;
use std::sync::{Arc, Once};

use poly_cache::{Namespace, ResultCache, SerializedArgs};
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use serde::Serialize;

use crate::config::{Config, EngineConfig, Kind};
use crate::discover::{DiscoveredFile, discover};
use crate::engine::{Diagnostic, Edit, Engine, FormatOutput, SourceFile};
use crate::engines::catalog_tool::CatalogToolEngine;
use crate::language::Language;
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

/// One engine paired with its resolved config and once-serialised cache args.
///
/// Built once per language (not per file) so the per-file rayon loop neither
/// rebuilds the engine list, re-resolves `EngineConfig`, nor re-serialises the
/// engine's options into the cache key — the latter was the per-file hot-path
/// cost this carries out of the loop.
struct EnginePlan {
    engine: Box<dyn Engine>,
    config: EngineConfig,
    serialized_args: SerializedArgs,
}

/// Resolve the engines (filtered to those with the requested capability) for a
/// language, pre-resolving each one's config and serialising its args once.
fn plan_engines(language: &Language, config: &Config, kind: Kind) -> Vec<EnginePlan> {
    let mut engines = engines_for(language);
    engines.extend(catalog_engines_for(language, config, kind));
    engines
        .into_iter()
        .filter(|engine| match kind {
            Kind::Lint => engine.capabilities().lint,
            Kind::Format => engine.capabilities().format,
        })
        .map(|engine| {
            let cfg = config.engine_config(language, engine.name(), kind);
            let serialized_args = ResultCache::serialize_args(&cfg.options);
            EnginePlan {
                engine,
                config: cfg,
                serialized_args,
            }
        })
        .collect()
}

/// Build the catalog-driven engines (ADR 0013) for `language`: one
/// [`CatalogToolEngine`] per enabled `[tools.<name>]` whose catalog tool both
/// declares a language that maps to `language` and exposes the capability for
/// `kind`. Catalog tools are format-only for now, so this is empty for
/// [`Kind::Lint`].
fn catalog_engines_for(language: &Language, config: &Config, kind: Kind) -> Vec<Box<dyn Engine>> {
    if kind != Kind::Format {
        return Vec::new();
    }
    let catalog = poly_catalog::Catalog::get();
    let mut engines: Vec<Box<dyn Engine>> = Vec::new();
    for (name, tool_config) in config.tools.iter() {
        if !tool_config.enabled {
            continue;
        }
        // Names are allowlist-validated at config load, so an absent entry here
        // is a defensive skip rather than an error.
        let Some(tool) = catalog.tool(name) else {
            continue;
        };
        let serves_language = tool
            .languages
            .iter()
            .any(|catalog_lang| &Language::from_catalog_name(catalog_lang) == language);
        if !serves_language {
            continue;
        }
        if let Some(engine) = CatalogToolEngine::format_engine(
            tool,
            tool_config.command.as_deref(),
            tool_config.args.as_deref(),
        ) {
            engines.push(Box::new(engine));
        }
    }
    engines
}

/// Build a per-language engine plan covering every language present in `files`,
/// so each distinct language is planned exactly once before the file loop.
fn plan_by_language(
    files: &[DiscoveredFile],
    config: &Config,
    kind: Kind,
) -> FxHashMap<Language, Vec<EnginePlan>> {
    // `Language` is a small enum key; FxHashMap's fast non-cryptographic hash
    // beats std SipHash here, and this lookup runs once per file × engine pass.
    let mut plans: FxHashMap<Language, Vec<EnginePlan>> = FxHashMap::default();
    for f in files {
        plans
            .entry(f.language.clone())
            .or_insert_with(|| plan_engines(&f.language, config, kind));
    }
    plans
}

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
    let cache = ResultCache::open_default(!opts.no_cache)?;
    let files = discover(paths);
    let plans = plan_by_language(&files, config, Kind::Lint);
    let mut results: Vec<LintResult> = files
        .par_iter()
        .filter_map(|f| match lint_one(f, &plans, &cache, fix) {
            Ok(result) => Some(result),
            // A per-file failure (read, parse, or — when fixing — the atomic
            // write) must not be swallowed silently; surface it and skip the file.
            Err(error) => {
                eprintln!("warning: {}: {error:#}", f.path.display());
                None
            }
        })
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
    let cache = ResultCache::open_default(!opts.no_cache)?;
    let files = discover(paths);
    let plans = plan_by_language(&files, config, Kind::Format);
    let mut results: Vec<FormatResult> = files
        .par_iter()
        .filter_map(|f| match format_one(f, &plans, &cache, write) {
            Ok(result) => Some(result),
            // A per-file failure (read, engine, or — when writing — the atomic
            // rename) must not be swallowed silently; surface it and skip the file.
            Err(error) => {
                eprintln!("warning: {}: {error:#}", f.path.display());
                None
            }
        })
        .collect();
    results.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(results)
}

fn lint_one(
    f: &DiscoveredFile,
    plans: &FxHashMap<Language, Vec<EnginePlan>>,
    cache: &ResultCache,
    fix: bool,
) -> anyhow::Result<LintResult> {
    let original = std::fs::read_to_string(&f.path)?;
    let mut diagnostics = lint_content(f, plans, cache, &original)?;

    if fix {
        // Only the fix path mutates the buffer, so clone lazily here; a plain
        // (no-fix) lint borrows `original` and never copies the file.
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
                    diagnostics = lint_content(f, plans, cache, &content)?;
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
    plans: &FxHashMap<Language, Vec<EnginePlan>>,
    cache: &ResultCache,
    content: &str,
) -> anyhow::Result<Vec<Diagnostic>> {
    let src = SourceFile {
        path: f.path.clone(),
        language: f.language.clone(),
        content: Arc::from(content),
    };
    // Content is constant across this file's engines, so digest it once.
    let digest = ResultCache::single_file_digest(content);
    let mut all = Vec::new();
    let engine_plans = plans.get(&f.language).map(Vec::as_slice).unwrap_or(&[]);
    for plan in engine_plans {
        let key = ResultCache::key_with_args(
            Namespace::Lint,
            plan.engine.name(),
            plan.engine.version(),
            &plan.serialized_args,
            &digest,
        );
        if let Some(bytes) = cache.get(Namespace::Lint, &key)
            && let Ok(diags) = serde_json::from_slice::<Vec<Diagnostic>>(&bytes)
        {
            all.extend(diags);
            continue;
        }
        let diags = plan.engine.lint(&src, &plan.config)?;
        if let Ok(bytes) = serde_json::to_vec(&diags)
            && let Err(error) = cache.put(Namespace::Lint, &key, &bytes)
        {
            tracing::warn!(
                engine = plan.engine.name(),
                "failed to store lint cache entry: {error:#}"
            );
        }
        all.extend(diags);
    }
    Ok(all)
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
    // Step 1 — filter groups with internal overlaps; sort remaining groups
    // rightmost-first (by the highest end_byte in the group).
    let mut valid: Vec<&[Edit]> = edit_groups
        .iter()
        .copied()
        .filter(|g| !g.is_empty() && !has_internal_overlap(g))
        .collect();
    valid.sort_by_key(|g| std::cmp::Reverse(g.iter().map(|e| e.end_byte).max().unwrap_or(0)));

    let mut result = content.to_string();
    // `prev_start` = leftmost start_byte committed so far.  Any edit whose
    // end_byte exceeds `prev_start` would overlap an already-committed range.
    let mut prev_start = usize::MAX;
    let mut applied = false;

    'groups: for group in &valid {
        // Validate every edit in the group against the current result length
        // and the committed boundary.
        for e in *group {
            if e.start_byte > e.end_byte || e.end_byte > result.len() || e.end_byte > prev_start {
                continue 'groups;
            }
            if !result.is_char_boundary(e.start_byte) || !result.is_char_boundary(e.end_byte) {
                continue 'groups;
            }
        }

        // Group is safe — apply its edits right-to-left within the group. The
        // single-edit case (every backend today) skips the sort allocation.
        if let [e] = *group {
            result.replace_range(e.start_byte..e.end_byte, &e.replacement);
        } else {
            let mut ordered: Vec<&Edit> = group.iter().collect();
            ordered.sort_by_key(|e| std::cmp::Reverse(e.start_byte));
            for e in &ordered {
                result.replace_range(e.start_byte..e.end_byte, &e.replacement);
            }
        }

        // Advance the committed boundary to the leftmost start in this group.
        prev_start = group
            .iter()
            .map(|e| e.start_byte)
            .min()
            .unwrap_or(prev_start);
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
            // Ranges intersect, or two zero-width insertions land on the same
            // byte (order between them would be ambiguous).
            let intersects = a.start_byte < b.end_byte && b.start_byte < a.end_byte;
            let same_point_insert = a.start_byte == a.end_byte
                && b.start_byte == b.end_byte
                && a.start_byte == b.start_byte;
            if intersects || same_point_insert {
                return true;
            }
        }
    }
    false
}

fn format_one(
    f: &DiscoveredFile,
    plans: &FxHashMap<Language, Vec<EnginePlan>>,
    cache: &ResultCache,
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
    let engine_plans = plans.get(&f.language).map(Vec::as_slice).unwrap_or(&[]);
    for plan in engine_plans {
        // Each engine's output feeds the next, so the digest is recomputed from
        // the current text; the args, however, were serialised once per engine.
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
            current = Arc::from(text);
            continue;
        }
        src.content = Arc::clone(&current);
        let out: Arc<str> = match plan.engine.format(&src, &plan.config)? {
            FormatOutput::Unchanged => Arc::clone(&current),
            FormatOutput::Formatted(s) => Arc::from(s),
        };
        if let Err(error) = cache.put(Namespace::Fmt, &key, out.as_bytes()) {
            tracing::warn!(
                engine = plan.engine.name(),
                "failed to store fmt cache entry: {error:#}"
            );
        }
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
    // On rename failure (e.g. cross-device, permissions) the sibling tmp would
    // otherwise be orphaned in the working tree; clean it up best-effort.
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
        // Ignore error: the global pool may already be initialized by a caller.
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

    // ── apply_edits ──────────────────────────────────────────────────────────

    /// Two diagnostics, each with two non-overlapping edits; all four apply.
    #[test]
    fn multi_edit_two_groups_apply_atomically() {
        // content: "hello world foo"
        //           0123456789012345
        let content = "hello world foo";
        // Group A: replace "world" (6..11) → "earth", replace "foo" (12..15) → "bar"
        let group_a = vec![edit(6, 11, "earth"), edit(12, 15, "bar")];
        // Group B: replace "hello" (0..5) → "hey"
        let group_b = vec![edit(0, 5, "hey")];

        let result = apply_edits(content, &[group_a.as_slice(), group_b.as_slice()])
            .expect("should produce output");
        assert_eq!(result, "hey earth bar");
    }

    /// A diagnostic whose edits overlap each other is skipped entirely.
    #[test]
    fn overlapping_edits_within_group_are_skipped() {
        let content = "abcdefgh";
        // Overlapping: [2..6) and [4..8) share bytes 4–6.
        let bad_group = vec![edit(2, 6, "X"), edit(4, 8, "Y")];

        let result = apply_edits(content, &[bad_group.as_slice()]);
        assert!(result.is_none(), "overlapping group must produce no output");
    }

    /// When two groups from different diagnostics conflict, the leftward group
    /// is deferred (not corrupted).
    #[test]
    fn cross_group_conflict_defers_leftward_group() {
        // content: "abcde"
        // Group A (rightmost): replace [3..5) → "XX"
        // Group B (leftward, overlapping): replace [2..4) → "YY" — conflicts with A
        let content = "abcde";
        let group_a = vec![edit(3, 5, "XX")];
        let group_b = vec![edit(2, 4, "YY")];

        let result = apply_edits(content, &[group_a.as_slice(), group_b.as_slice()])
            .expect("should produce output from group A");
        // Group A applies, group B is skipped.
        assert_eq!(result, "abcXX");
    }

    // ── has_internal_overlap ─────────────────────────────────────────────────

    #[test]
    fn non_overlapping_edits_pass_internal_check() {
        let group = vec![edit(0, 5, "a"), edit(5, 10, "b")];
        assert!(!has_internal_overlap(&group));
    }

    #[test]
    fn adjacent_edits_are_not_overlapping() {
        // [0,5) and [5,10) share no bytes (end is exclusive).
        let group = vec![edit(0, 5, "a"), edit(5, 10, "b")];
        assert!(!has_internal_overlap(&group));
    }

    #[test]
    fn touching_edits_with_overlap_detected() {
        // [0,6) and [4,10) overlap at bytes 4–6.
        let group = vec![edit(0, 6, "a"), edit(4, 10, "b")];
        assert!(has_internal_overlap(&group));
    }

    // ── worker stack size ────────────────────────────────────────────────────

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
