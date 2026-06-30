//! Parallel orchestration (rayon): discover files, route to backends, run with
//! content-hash caching, collect results. Defaults to all logical cores.

use std::path::PathBuf;
use std::sync::{Arc, Once};

use poly_cache::{Namespace, ResultCache, SerializedArgs};
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
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

/// Per-engine debug record for one file. Collected only when debug output is
/// requested (`--debug`); never built on the default hot path.
#[derive(Debug, Clone, Serialize)]
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
pub struct RunDebug {
    /// One entry per engine evaluated for the file.
    pub engines: Vec<EngineDebug>,
}

/// Per-file lint outcome.
#[derive(Debug, Clone, Serialize)]
pub struct LintResult {
    /// File that was linted.
    pub path: PathBuf,
    /// Diagnostics from all backends for this file.
    pub diagnostics: Vec<Diagnostic>,
    /// Debug data (cache hit/miss + timing), present only under `--debug`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debug: Option<RunDebug>,
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
    /// Debug data (cache hit/miss + timing), present only under `--debug`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debug: Option<RunDebug>,
}

/// Maximum autofix passes per file: applying a fix can surface or resolve
/// others, so re-lint until stable, but cap to guarantee termination.
const MAX_FIX_PASSES: usize = 5;

/// Minimum engine runtime for a result to be worth caching.
///
/// Persisting a result costs a serialize + atomic temp-write + rename, and
/// reading it back costs an open + read + deserialize. When an engine produced a
/// result in less time than this, that round-trip is slower than just recomputing
/// it — and on a whole-repository run the overwhelming majority of files fall
/// here, so unconditionally caching them made a cold run several times slower than
/// `--no-cache` (one tiny cache file per file × engine). Only results that took at
/// least this long are persisted; cheaper ones are recomputed every run, which is
/// by construction fast. Genuinely expensive files (large/slow inputs) still get
/// cached and skip the work on the next run.
const MIN_CACHE_DURATION: std::time::Duration = std::time::Duration::from_millis(5);

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
/// declares a language that maps to `language` and exposes a usable command for
/// `kind`.
///
/// [`Kind::Format`] wires the tool's format command; [`Kind::Lint`] wires its
/// lint command — but only when that command is **non-mutating** (a `--fix` /
/// `--write` / `-w` / `-i` command would corrupt files if run as a linter, so
/// [`CatalogToolEngine::lint_engine`] skips it). Catalog linting is a
/// best-effort, breadth-tier mechanism (file-level, exit-code based); structured
/// per-tool diagnostics remain the curated native backends' job.
fn catalog_engines_for(language: &Language, config: &Config, kind: Kind) -> Vec<Box<dyn Engine>> {
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
        let command = tool_config.command.as_deref();
        let args = tool_config.args.as_deref();
        let engine = match kind {
            Kind::Format => CatalogToolEngine::format_engine(tool, command, args),
            Kind::Lint => CatalogToolEngine::lint_engine(tool, command, args),
        };
        if let Some(engine) = engine {
            engines.push(Box::new(engine));
        }
    }
    engines
}

/// Warm the tree-sitter-language-pack grammars the generic (tier-2) backend will
/// need, in one pass before the rayon loop, so the hot loop only parses — never
/// downloads or `dlopen`s a grammar under contention. Only grammars for files
/// routed to the `treesitter` engine are prefetched (tier-1 languages handled by
/// a native backend never touch the pack). A failure is non-fatal: the per-file
/// path still lazily loads each grammar on first use.
fn prefetch_tier2_grammars(plans: &FxHashMap<Language, Vec<EnginePlan>>) {
    let grammars: Vec<&str> = plans
        .iter()
        .filter(|(_, engine_plans)| {
            engine_plans
                .iter()
                .any(|plan| plan.engine.name() == "treesitter")
        })
        .filter_map(|(language, _)| match language {
            // The generic tier keys off the pack's grammar id, which is exactly
            // the `Language::Other` payload (set by discovery via the pack's own
            // path detection).
            Language::Other(name) => Some(name.as_str()),
            _ => None,
        })
        .collect();
    if grammars.is_empty() {
        return;
    }
    if let Err(error) = tree_sitter_language_pack::prefetch(&grammars) {
        tracing::warn!(%error, "tier-2 grammar prefetch failed; falling back to lazy load");
    }
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
    collect_debug: bool,
) -> anyhow::Result<Vec<LintResult>> {
    configure_pool(opts.jobs);
    let cache = ResultCache::open_default(!opts.no_cache)?;
    let files = discover(paths, &config.exclude);
    let plans = plan_by_language(&files, config, Kind::Lint);
    prefetch_tier2_grammars(&plans);
    let mut results: Vec<LintResult> = files
        .par_iter()
        .filter_map(|f| match lint_one(f, &plans, &cache, fix, collect_debug) {
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
    collect_debug: bool,
) -> anyhow::Result<Vec<FormatResult>> {
    configure_pool(opts.jobs);
    let cache = ResultCache::open_default(!opts.no_cache)?;
    // Generated lock files are machine-maintained; reformatting them (taplo over
    // Cargo.lock, the YAML formatter over pnpm-lock.yaml, …) corrupts them. Skip
    // them on a directory walk so a stray `poly fmt .` is safe — but still honour
    // a lock file passed explicitly as a path argument.
    let explicit: FxHashSet<&std::path::Path> = paths.iter().map(PathBuf::as_path).collect();
    let files: Vec<DiscoveredFile> = discover(paths, &config.exclude)
        .into_iter()
        .filter(|f| explicit.contains(f.path.as_path()) || !is_generated_lockfile(&f.path))
        .collect();
    let plans = plan_by_language(&files, config, Kind::Format);
    prefetch_tier2_grammars(&plans);
    let mut results: Vec<FormatResult> = files
        .par_iter()
        .filter_map(
            |f| match format_one(f, &plans, &cache, write, collect_debug) {
                Ok(result) => Some(result),
                // A per-file failure (read, engine, or — when writing — the atomic
                // rename) must not be swallowed silently; surface it and skip the file.
                Err(error) => {
                    eprintln!("warning: {}: {error:#}", f.path.display());
                    None
                }
            },
        )
        .collect();
    results.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(results)
}

/// Generated lock files, by exact name, that `poly fmt` never rewrites on a
/// directory walk. Any `*.lock` file is also treated as a lock file; these are
/// the ones whose names do not end in `.lock`.
const LOCKFILE_NAMES: &[&str] = &[
    "package-lock.json",
    "npm-shrinkwrap.json",
    "pnpm-lock.yaml",
    "bun.lockb",
];

/// Whether `path` is a machine-generated lock file that must not be reformatted.
/// Matched by the `*.lock` extension (Cargo.lock, yarn.lock, poetry.lock,
/// uv.lock, composer.lock, Gemfile.lock, flake.lock, deno.lock, …) or by an
/// exact name in [`LOCKFILE_NAMES`] for the lock files that don't end in `.lock`.
fn is_generated_lockfile(path: &std::path::Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    name.ends_with(".lock") || LOCKFILE_NAMES.contains(&name)
}

fn lint_one(
    f: &DiscoveredFile,
    plans: &FxHashMap<Language, Vec<EnginePlan>>,
    cache: &ResultCache,
    fix: bool,
    collect_debug: bool,
) -> anyhow::Result<LintResult> {
    let original = std::fs::read_to_string(&f.path)?;
    let (mut diagnostics, mut debug) = lint_content(f, plans, cache, &original, collect_debug)?;

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
                    let (next_diags, next_debug) =
                        lint_content(f, plans, cache, &content, collect_debug)?;
                    diagnostics = next_diags;
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
        debug,
    })
}

/// Run every lint-capable engine for the file's language over `content`,
/// content-hash caching each engine's diagnostics. When `collect_debug` is set,
/// also returns per-engine cache hit/miss + timing; otherwise the second tuple
/// element is `None` and no timing instrumentation runs.
fn lint_content(
    f: &DiscoveredFile,
    plans: &FxHashMap<Language, Vec<EnginePlan>>,
    cache: &ResultCache,
    content: &str,
    collect_debug: bool,
) -> anyhow::Result<(Vec<Diagnostic>, Option<RunDebug>)> {
    let src = SourceFile {
        path: f.path.clone(),
        language: f.language.clone(),
        content: Arc::from(content),
    };
    // The digest (and the per-engine keys derived from it) are only needed to
    // address the cache, so skip them entirely when the cache is disabled.
    let digest = cache
        .enabled()
        .then(|| ResultCache::single_file_digest(content));
    let mut all = Vec::new();
    let mut debug = collect_debug.then(RunDebug::default);
    let engine_plans = plans.get(&f.language).map(Vec::as_slice).unwrap_or(&[]);
    for plan in engine_plans {
        let key = digest.as_ref().map(|digest| {
            ResultCache::key_with_args(
                Namespace::Lint,
                plan.engine.name(),
                plan.engine.version(),
                &plan.serialized_args,
                digest,
            )
        });
        if let Some(key) = &key
            && let Some(bytes) = cache.get(Namespace::Lint, key)
            && let Ok(diags) = serde_json::from_slice::<Vec<Diagnostic>>(&bytes)
        {
            push_engine_debug(debug.as_mut(), plan, None);
            all.extend(diags);
            continue;
        }
        let started = std::time::Instant::now();
        let diags = plan.engine.lint(&src, &plan.config)?;
        let elapsed = started.elapsed();
        note_slow_engine(&f.path, content.len(), plan.engine.name(), elapsed);
        push_engine_debug(debug.as_mut(), plan, Some(started));
        // Persist only results expensive enough that reloading beats recomputing
        // (see MIN_CACHE_DURATION).
        if let Some(key) = &key
            && elapsed >= MIN_CACHE_DURATION
            && let Ok(bytes) = serde_json::to_vec(&diags)
            && let Err(error) = cache.put(Namespace::Lint, key, &bytes)
        {
            tracing::warn!(
                engine = plan.engine.name(),
                "failed to store lint cache entry: {error:#}"
            );
        }
        all.extend(diags);
    }
    Ok((all, debug))
}

/// Append one [`EngineDebug`] record when debug collection is active. `started`
/// is `Some` for an engine that actually ran (timing it) and `None` for a cache
/// hit (`duration_ms` = 0, `cache_hit` = true).
fn push_engine_debug(
    debug: Option<&mut RunDebug>,
    plan: &EnginePlan,
    started: Option<std::time::Instant>,
) {
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

/// A single engine taking at least this long on one file is surfaced at `warn`:
/// at this scale it is almost always a pathological input for that backend (a
/// huge generated file, or a backend with super-linear behaviour on certain
/// shapes), and it serialises a whole-repo run on one core.
const SLOW_ENGINE_WARN: std::time::Duration = std::time::Duration::from_secs(2);

/// A run at least this long is surfaced at `info` (visible under `-v`), so the
/// per-file cost is observable before it reaches the `warn` threshold.
const SLOW_ENGINE_INFO: std::time::Duration = std::time::Duration::from_millis(250);

/// The generic tree-sitter tier; slow runs here are our own code, not a wrapped
/// upstream tool, so the upstream-issue hint is omitted for it.
const GENERIC_TIER_ENGINE: &str = "treesitter";

/// Surface a slow single-file engine run, noting the file, its size, the backend,
/// and the elapsed time. For a wrapped upstream tool (anything but the generic
/// tree-sitter tier) the message suggests reporting it upstream, since the cost
/// is in that tool, not in polylint.
fn note_slow_engine(
    path: &std::path::Path,
    bytes: usize,
    engine: &str,
    elapsed: std::time::Duration,
) {
    // tracing's Value impls cover u64 but not u128/usize, so normalise here.
    let bytes = bytes as u64;
    let elapsed_ms = elapsed.as_millis() as u64;
    let file = path.display();
    if elapsed >= SLOW_ENGINE_WARN {
        if engine == GENERIC_TIER_ENGINE {
            tracing::warn!(
                %file,
                bytes,
                engine,
                elapsed_ms,
                "slow on a large file (generic tree-sitter tier)"
            );
        } else {
            tracing::warn!(
                %file,
                bytes,
                engine,
                elapsed_ms,
                "slow on this file; the cost is in the `{engine}` backend — \
                 consider reporting it upstream to the {engine} project"
            );
        }
    } else if elapsed >= SLOW_ENGINE_INFO {
        tracing::info!(
            %file,
            bytes,
            engine,
            elapsed_ms,
            "engine spent notable time on this file"
        );
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
    collect_debug: bool,
) -> anyhow::Result<FormatResult> {
    let original = std::fs::read_to_string(&f.path)?;
    let mut debug = collect_debug.then(RunDebug::default);
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
    let cache_enabled = cache.enabled();
    for plan in engine_plans {
        // Each engine's output feeds the next, so the digest is recomputed from
        // the current text; the args, however, were serialised once per engine.
        // Only needed to address the cache, so skip it when the cache is off.
        let key = cache_enabled.then(|| {
            let digest = ResultCache::single_file_digest(&current);
            ResultCache::key_with_args(
                Namespace::Fmt,
                plan.engine.name(),
                plan.engine.version(),
                &plan.serialized_args,
                &digest,
            )
        });
        if let Some(key) = &key
            && let Some(bytes) = cache.get(Namespace::Fmt, key)
            && let Ok(text) = String::from_utf8(bytes)
        {
            push_engine_debug(debug.as_mut(), plan, None);
            current = Arc::from(text);
            continue;
        }
        src.content = Arc::clone(&current);
        let started = std::time::Instant::now();
        let out: Arc<str> = match plan.engine.format(&src, &plan.config)? {
            FormatOutput::Unchanged => Arc::clone(&current),
            FormatOutput::Formatted(s) => Arc::from(s),
        };
        let elapsed = started.elapsed();
        note_slow_engine(&f.path, src.content.len(), plan.engine.name(), elapsed);
        push_engine_debug(debug.as_mut(), plan, Some(started));
        // Persist only results expensive enough that reloading beats recomputing
        // (see MIN_CACHE_DURATION).
        if let Some(key) = &key
            && elapsed >= MIN_CACHE_DURATION
            && let Err(error) = cache.put(Namespace::Fmt, key, out.as_bytes())
        {
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
        debug,
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

    /// A lint engine that sleeps a fixed duration, used to exercise the
    /// duration-gated cache-write policy ([`MIN_CACHE_DURATION`]).
    struct TimedEngine {
        name: &'static str,
        delay: std::time::Duration,
    }

    impl Engine for TimedEngine {
        fn name(&self) -> &'static str {
            self.name
        }
        fn languages(&self) -> &'static [Language] {
            &[]
        }
        fn capabilities(&self) -> crate::engine::Capabilities {
            crate::engine::Capabilities {
                lint: true,
                format: false,
                fix: false,
            }
        }
        fn version(&self) -> &str {
            "1"
        }
        fn lint(&self, _src: &SourceFile, _cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
            std::thread::sleep(self.delay);
            Ok(Vec::new())
        }
    }

    /// Only results whose engine ran at least [`MIN_CACHE_DURATION`] are
    /// persisted: a cheap result is recomputed each run (caching it cost more
    /// than recomputing), while an expensive one is cached even when it produced
    /// no diagnostics (the gate is on cost, not on emptiness).
    #[test]
    fn caches_only_results_above_duration_threshold() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = ResultCache::open(tmp.path().join("cache"), true).expect("open cache");
        let config = Config::default();
        let content = "x = 1\n";
        let language = Language::Python;

        let empty_args = ResultCache::serialize_args(&toml::Table::new());
        let key_for = |name: &str| {
            let digest = ResultCache::single_file_digest(content);
            ResultCache::key_with_args(Namespace::Lint, name, "1", &empty_args, &digest)
        };
        let plan = |name: &'static str, delay| EnginePlan {
            engine: Box::new(TimedEngine { name, delay }),
            config: config.engine_config(&language, name, Kind::Lint),
            serialized_args: ResultCache::serialize_args(&toml::Table::new()),
        };
        let file = DiscoveredFile {
            path: std::path::PathBuf::from("mem.py"),
            language: language.clone(),
        };

        // Fast engine (no delay): below threshold → must not be cached.
        let mut plans: FxHashMap<Language, Vec<EnginePlan>> = FxHashMap::default();
        plans.insert(
            language.clone(),
            vec![plan("fast-eng", std::time::Duration::ZERO)],
        );
        lint_content(&file, &plans, &cache, content, false).expect("lint fast");
        assert!(
            cache.get(Namespace::Lint, &key_for("fast-eng")).is_none(),
            "a sub-threshold (cheap) result must not be cached"
        );

        // Slow engine (> threshold): must be cached despite producing no diagnostics.
        let mut plans: FxHashMap<Language, Vec<EnginePlan>> = FxHashMap::default();
        plans.insert(
            language.clone(),
            vec![plan(
                "slow-eng",
                MIN_CACHE_DURATION + std::time::Duration::from_millis(20),
            )],
        );
        lint_content(&file, &plans, &cache, content, false).expect("lint slow");
        assert!(
            cache.get(Namespace::Lint, &key_for("slow-eng")).is_some(),
            "an above-threshold (expensive) result must be cached"
        );
    }

    #[test]
    fn recognizes_generated_lock_files() {
        for name in [
            "Cargo.lock",
            "yarn.lock",
            "poetry.lock",
            "uv.lock",
            "Gemfile.lock",
            "flake.lock",
            "composer.lock",
            "package-lock.json",
            "pnpm-lock.yaml",
            "npm-shrinkwrap.json",
            "bun.lockb",
        ] {
            assert!(
                is_generated_lockfile(std::path::Path::new(name)),
                "{name} should be treated as a lock file"
            );
        }
        for name in ["main.rs", "Cargo.toml", "package.json", "lockfile.txt"] {
            assert!(
                !is_generated_lockfile(std::path::Path::new(name)),
                "{name} must not be treated as a lock file"
            );
        }
    }

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
