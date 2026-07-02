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
use crate::engines::rule_config::RuleSelection;
use crate::filter::{PerFileIgnores, SeverityRemap, is_generated_lockfile, match_bases, relative_for_match};
use crate::language::Language;
use crate::registry::engines_for;
use crate::resolve::ConfigSet;

/// A per-file engine plan map, keyed by `(config_id, language)` so a monorepo's
/// nested configs each get their own plans (ADR 0018). In a single-config repo
/// every file shares `config_id == 0`, collapsing to one plan per language.
type PlanMap = FxHashMap<(usize, Language), Vec<EnginePlan>>;

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
    /// When `true`, the caller supplied an explicit `--config <path>`: use that
    /// single config for every file and skip hierarchical (nested `poly.toml`)
    /// resolution (ADR 0018). Default `false` — scan for nested configs.
    pub explicit_config: bool,
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
    /// Per-rule severity overrides for this engine, compiled once. Applied to
    /// this plan's diagnostics only — never globally — so one engine's rule code
    /// cannot remap another engine's identically-named code.
    severity_remap: SeverityRemap,
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
            let serialized_args = ResultCache::serialize_args(&cache_args(&cfg));
            let severity_remap = build_severity_remap(&cfg);
            EnginePlan {
                engine,
                config: cfg,
                serialized_args,
                severity_remap,
            }
        })
        .collect()
}

/// Compile this engine's per-rule severity overrides from its resolved config:
/// the `[lint.<lang>.<tool>.rules.<code>] level` entries where a level was set.
/// Applied uniformly as a post-lint remap, so an engine with no native severity
/// config still honors a configured `level`.
fn build_severity_remap(cfg: &EngineConfig) -> SeverityRemap {
    let selection = RuleSelection::from_options(cfg);
    let entries = selection
        .rules
        .into_iter()
        .filter_map(|(code, opts)| opts.level.map(|level| (code, level)))
        .collect();
    SeverityRemap::new(entries)
}

/// The args table folded into the cache key for an engine: the user's per-engine
/// `options` PLUS the effective `[defaults]` globals + indent width under
/// reserved `__`-prefixed keys. Without the globals, changing `[defaults]
/// line_length` (etc.) would not invalidate cached output, since most engines
/// read those from globals rather than their own options table.
fn cache_args(cfg: &EngineConfig) -> toml::Table {
    let mut table = cfg.options.clone();
    table.insert(
        "__globals_line_length".to_string(),
        toml::Value::Integer(cfg.globals.line_length as i64),
    );
    table.insert(
        "__globals_line_ending".to_string(),
        toml::Value::String(format!("{:?}", cfg.globals.line_ending)),
    );
    table.insert(
        "__globals_final_newline".to_string(),
        toml::Value::Boolean(cfg.globals.final_newline),
    );
    table.insert(
        "__globals_trim_trailing_whitespace".to_string(),
        toml::Value::Boolean(cfg.globals.trim_trailing_whitespace),
    );
    table.insert(
        "__indent_width".to_string(),
        toml::Value::Integer(cfg.indent_width as i64),
    );
    table
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
        let env = tool_config.env.clone();
        let root = tool_config.root.as_ref().map(std::path::PathBuf::from);
        let engine = match kind {
            Kind::Format => CatalogToolEngine::format_engine(tool, command, args, env, root),
            Kind::Lint => CatalogToolEngine::lint_engine(tool, command, args, env, root),
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
fn prefetch_tier2_grammars(plans: &PlanMap) {
    let grammars: FxHashSet<&str> = plans
        .iter()
        .filter(|(_, engine_plans)| engine_plans.iter().any(|plan| plan.engine.name() == "treesitter"))
        .filter_map(|((_, language), _)| match language {
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
    let grammars: Vec<&str> = grammars.into_iter().collect();
    if let Err(error) = tree_sitter_language_pack::prefetch(&grammars) {
        tracing::warn!(%error, "tier-2 grammar prefetch failed; falling back to lazy load");
    }
}

/// Build the engine plan for every `(config_id, language)` pair present in
/// `files`, so each distinct pair is planned exactly once before the file loop.
/// A nested config and the root config plan independently even for the same
/// language, since their resolved options differ (ADR 0018).
fn plan_by_config_language(files: &[DiscoveredFile], configs: &ConfigSet, kind: Kind) -> PlanMap {
    let mut plans: PlanMap = FxHashMap::default();
    for f in files {
        plans
            .entry((f.config_id, f.language.clone()))
            .or_insert_with(|| plan_engines(&f.language, configs.config(f.config_id), kind));
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
    let configs = build_config_set(paths, config, opts)?;
    let files = discover(paths, &configs, &opts.exclude);
    let plans = plan_by_config_language(&files, &configs, Kind::Lint);
    prefetch_tier2_grammars(&plans);
    // One compiled `[per-file-ignores]` matcher per resolved config, indexed by
    // `config_id` (nested configs suppress rules for their own subtree only).
    let ignores: Vec<PerFileIgnores> = configs
        .iter()
        .map(|c| PerFileIgnores::compile(&c.per_file_ignores))
        .collect();
    let bases = match_bases(paths);
    let mut results: Vec<LintResult> = files
        .par_iter()
        .filter_map(
            |f| match lint_one(f, &plans, &cache, fix, collect_debug, &configs, &ignores, &bases) {
                Ok(result) => Some(result),
                // A per-file failure (read, parse, or — when fixing — the atomic
                // write) must not be swallowed silently; surface it and skip the file.
                Err(error) => {
                    tracing::warn!(path = %f.path.display(), "lint failed: {error:#}");
                    None
                }
            },
        )
        .filter(|r| !r.diagnostics.is_empty())
        .collect();
    results.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(results)
}

/// Build the run's [`ConfigSet`]: a single explicit config (`--config`) bypasses
/// hierarchical resolution; otherwise `config` is the root and the walked paths
/// are scanned for nested `poly.toml` files (ADR 0018).
fn build_config_set(paths: &[PathBuf], config: &Config, opts: &RunOptions) -> anyhow::Result<ConfigSet> {
    if opts.explicit_config {
        Ok(ConfigSet::single(config.clone()))
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
    configure_pool(opts.jobs);
    let cache = ResultCache::open_default(!opts.no_cache)?;
    // Generated lock files are machine-maintained; reformatting them (taplo over
    // Cargo.lock, the YAML formatter over pnpm-lock.yaml, …) corrupts them. Skip
    // them on a directory walk so a stray `poly fmt .` is safe — but still honour
    // a lock file passed explicitly as a path argument.
    let explicit: FxHashSet<&std::path::Path> = paths.iter().map(PathBuf::as_path).collect();
    let configs = build_config_set(paths, config, opts)?;
    let files: Vec<DiscoveredFile> = discover(paths, &configs, &opts.exclude)
        .into_iter()
        .filter(|f| explicit.contains(f.path.as_path()) || !is_generated_lockfile(&f.path))
        .collect();
    let plans = plan_by_config_language(&files, &configs, Kind::Format);
    prefetch_tier2_grammars(&plans);
    let mut results: Vec<FormatResult> = files
        .par_iter()
        .filter_map(|f| match format_one(f, &plans, &cache, write, collect_debug) {
            Ok(result) => Some(result),
            // A per-file failure (read, engine, or — when writing — the atomic
            // rename) must not be swallowed silently; surface it and skip the file.
            Err(error) => {
                tracing::warn!(path = %f.path.display(), "format failed: {error:#}");
                None
            }
        })
        .collect();
    results.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(results)
}

#[allow(clippy::too_many_arguments)] // cohesive per-file lint inputs; splitting would obscure the flow
fn lint_one(
    f: &DiscoveredFile,
    plans: &PlanMap,
    cache: &ResultCache,
    fix: bool,
    collect_debug: bool,
    configs: &ConfigSet,
    ignores: &[PerFileIgnores],
    bases: &[PathBuf],
) -> anyhow::Result<LintResult> {
    let original = std::fs::read_to_string(&f.path)?;
    // Per-file-ignored rules are suppressed *before* the fix loop so `--fix` does
    // not silently rewrite a file for a rule the user configured to ignore
    // (ruff's semantics: ignored rules never fire for the matching file). The
    // matcher and its base directories are the file's own config (ADR 0018).
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
    // Content is constant across this file's engines, so digest it once. The
    // path is folded in because lint diagnostics can depend on it (ruff INP001
    // message, isort package classification) — without it, byte-identical files
    // would share a cache entry and be served each other's path-bearing results.
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
        // Time the engine only when debug collection is on (zero cost otherwise).
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
        // Remap after caching the raw diagnostics: apply on both the fresh and
        // cache-hit paths so a configured `level` is honored identically.
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
            // Ranges intersect, or two zero-width insertions land on the same
            // byte (order between them would be ambiguous).
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
    let engine_plans = plans
        .get(&(f.config_id, f.language.clone()))
        .map(Vec::as_slice)
        .unwrap_or(&[]);
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
            push_engine_debug(debug.as_mut(), plan, None);
            current = Arc::from(text);
            continue;
        }
        src.content = Arc::clone(&current);
        // Time the engine only when debug collection is on (zero cost otherwise).
        let started = collect_debug.then(std::time::Instant::now);
        let out: Arc<str> = match plan.engine.format(&src, &plan.config)? {
            FormatOutput::Unchanged => Arc::clone(&current),
            FormatOutput::Formatted(s) => Arc::from(s),
        };
        push_engine_debug(debug.as_mut(), plan, started);
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
        formatted: if changed { Some(current.to_string()) } else { None },
        debug,
    })
}

fn write_atomic(path: &std::path::Path, contents: &str) -> anyhow::Result<()> {
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("polyfmt");
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

        let result = apply_edits(content, &[group_a.as_slice(), group_b.as_slice()]).expect("should produce output");
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
