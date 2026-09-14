//! Engine planning: resolve which backends serve a `(config, language)` pair for
//! a given [`Kind`], and pre-compute everything the per-file rayon loop would
//! otherwise redo — resolved [`EngineConfig`], serialised cache args, and the
//! compiled per-rule severity remap.
//!
//! Split out of `runner.rs` so the runner keeps to the pipeline itself
//! (discover -> cache -> engine -> report) and planning stays one concern per
//! file.

use poly_cache::{ResultCache, SerializedArgs};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::{Config, EngineConfig, Kind};
use crate::discover::DiscoveredFile;
use crate::engine::{ENABLED_OPTION_KEY, Engine};
use crate::engines::catalog_tool::{CatalogToolEngine, LintRefusal};
use crate::engines::rule_config::RuleSelection;
use crate::filter::SeverityRemap;
use crate::language::Language;
use crate::registry::engines_for;
use crate::resolve::ConfigSet;
use crate::runner::selection::{self, EngineSelection};
use crate::runner::skips::Withdrawal;

/// A per-file engine plan map, keyed by `(config_id, language)` so a monorepo's
/// nested configs each get their own plans (ADR 0018). In a single-config repo
/// every file shares `config_id == 0`, collapsing to one plan per language.
pub(super) type PlanMap = FxHashMap<(usize, Language), Vec<EnginePlan>>;

/// The `(config, language)` pairs a caller instruction withdrew coverage from,
/// and which instruction did it.
///
/// Kept beside [`PlanMap`] rather than folded into it because it answers a
/// different question: not "what runs" but "why does nothing run". An empty
/// plan already meant "poly has no engine for this file type", and reporting
/// that about a file the caller switched off on purpose is false.
pub(super) type WithdrawnMap = FxHashMap<(usize, Language), Withdrawal>;

/// A run's engine plans, and why any `(config, language)` pair has none.
///
/// The two maps are keyed identically and are always read together — "what runs
/// on this file" and "why does nothing" are the same lookup asked twice — so
/// they travel as one value rather than as a pair of parameters threaded
/// through every per-file signature.
pub(super) struct RunPlan {
    plans: PlanMap,
    withdrawn: WithdrawnMap,
}

impl RunPlan {
    /// The engines planned for a file, or an empty slice when nothing is.
    pub(super) fn engines(&self, config_id: usize, language: &Language) -> &[EnginePlan] {
        self.plans
            .get(&(config_id, language.clone()))
            .map_or(&[], Vec::as_slice)
    }

    /// The instruction that withdrew this pair's engines, if one did.
    pub(super) fn withdrawal(&self, config_id: usize, language: &Language) -> Option<&Withdrawal> {
        self.withdrawn.get(&(config_id, language.clone()))
    }

    /// Whether anything is planned for this pair — the question
    /// `unmatched_explicit_paths` asks of a path named on the command line.
    pub(super) fn routes(&self, config_id: usize, language: &Language) -> bool {
        !self.engines(config_id, language).is_empty()
    }

    /// The `(config, language)` pairs with at least one planned engine, for the
    /// tier-2 grammar prefetch.
    pub(super) fn iter(&self) -> impl Iterator<Item = (&(usize, Language), &Vec<EnginePlan>)> {
        self.plans.iter()
    }
}

/// Name of the generic tree-sitter engine — the tier-2 formatting fallback for
/// languages with no dedicated backend. Matched by name so it can be dropped
/// when a catalog formatter takes over the language.
const TREE_SITTER_ENGINE: &str = "treesitter";

/// One engine paired with its resolved config and once-serialised cache args.
///
/// Built once per language (not per file) so the per-file rayon loop neither
/// rebuilds the engine list, re-resolves `EngineConfig`, nor re-serialises the
/// engine's options into the cache key — the latter was the per-file hot-path
/// cost this carries out of the loop.
pub(super) struct EnginePlan {
    pub(super) engine: Box<dyn Engine>,
    pub(super) config: EngineConfig,
    pub(super) serialized_args: SerializedArgs,
    /// Per-rule severity overrides for this engine, compiled once. Applied to
    /// this plan's diagnostics only — never globally — so one engine's rule code
    /// cannot remap another engine's identically-named code.
    pub(super) severity_remap: SeverityRemap,
    /// Whether this engine carries lint rules for the planned language
    /// ([`Engine::provides_language_lint`]), resolved once here because the
    /// answer can cost a `PATH` probe or a rule-pack load — neither of which
    /// belongs in the per-file loop that asks it.
    pub(super) provides_language_lint: bool,
}

/// Whether a catalog formatter takes over the language, displacing poly's generic
/// tree-sitter reindenter.
///
/// Formatters chain, so running both makes them fight: poly reindents, the external
/// tool reindents differently, and [`format_to_fixed_point`] never converges — the
/// behaviour observed with Elixir and `mix format`. Gated on the tool actually being
/// runnable, so a configured-but-missing binary leaves the fallback in place instead
/// of silently dropping all formatting for the language.
fn generic_formatter_superseded(kind: Kind, catalog: &[Box<dyn Engine>]) -> bool {
    kind == Kind::Format && catalog.iter().any(|engine| engine.supersedes_generic_formatter())
}

/// Resolve the engines (filtered to those with the requested capability) for a
/// language, pre-resolving each one's config and serialising its args once.
fn has_tier_one_formatter(engines: &[Box<dyn Engine>], language: &Language, config: &Config, kind: Kind) -> bool {
    kind == Kind::Format
        && engines.iter().any(|engine| {
            engine.name() != TREE_SITTER_ENGINE
                && engine.provides_language_format(language, &config.engine_config(language, engine.name(), kind))
        })
}

/// Drop the engines that lack the capability `kind` asks for, so the collision
/// resolution below only ever compares backends that would actually run.
fn retaining_capable(engines: Vec<Box<dyn Engine>>, kind: Kind) -> Vec<Box<dyn Engine>> {
    engines
        .into_iter()
        .filter(|engine| match kind {
            Kind::Lint => engine.capabilities().lint,
            Kind::Format => engine.capabilities().format,
        })
        .collect()
}

/// Whether the registry backend `builtin` should displace a catalog tool that
/// answers the same [`Engine::name`].
///
/// A catalog tool and a registry backend of the same name are the *same tool*
/// wrapped twice — `[tools.shellcheck]` and the built-in `shellcheck` engine
/// both run the `shellcheck` binary. The built-in is the higher-fidelity wrapper
/// (structured spans and rule codes, against the catalog tier's file-level,
/// exit-code-based finding), so it wins — but only when it is genuinely doing
/// the work. An opt-in native tool that is switched off or missing from `PATH`
/// lints nothing, and letting *that* displace the catalog engine would trade a
/// duplicated diagnostic for no diagnostic at all.
///
/// Both kinds ask the same question, of the capability being planned. Format
/// used to answer an unconditional `true` on the reasoning that
/// [`has_tier_one_formatter`] had already yielded the catalog to any registry
/// formatter — true only while that predicate counted disabled opt-in tools as
/// formatters. Now that it asks whether the backend would really format, a
/// `Kind::Format` collision can reach here with a built-in that does nothing,
/// and displacing the catalog tool with it would trade a working formatter for
/// none — the same trade the lint arm has always refused.
///
/// Costs a `PATH` probe or a rule-pack load, but `plan_engines` runs once per
/// (config, language) — never per file — so it stays out of the hot loop.
fn builtin_displaces_catalog_tool(builtin: &dyn Engine, language: &Language, config: &Config, kind: Kind) -> bool {
    let cfg = config.engine_config(language, builtin.name(), kind);
    match kind {
        Kind::Format => builtin.provides_language_format(language, &cfg),
        Kind::Lint => builtin.provides_language_lint(language, &cfg),
    }
}

/// Merge the catalog engines into the registry list, keeping **one engine per
/// name**: everything downstream of a plan — the `[<kind>.<lang>.<name>]` config
/// table, the compiled severity remap, and the `engine` field of every reported
/// diagnostic — is keyed by name alone, so two entries sharing one would be
/// indistinguishable to config, to a JSON consumer, and to the reader of a
/// doubled finding.
///
/// The survivor is whichever of the pair actually does the work: the registry
/// backend when [`builtin_displaces_catalog_tool`] holds, otherwise the catalog
/// engine — and then the inert built-in is dropped, since keeping a backend that
/// yields nothing would leave the collision in place for no gain.
fn merge_catalog_engines(
    language: &Language,
    config: &Config,
    kind: Kind,
    mut engines: Vec<Box<dyn Engine>>,
    mut catalog: Vec<Box<dyn Engine>>,
) -> Vec<Box<dyn Engine>> {
    catalog.retain(|tool| {
        let Some(builtin) = engines.iter().find(|engine| engine.name() == tool.name()) else {
            return true;
        };
        if !builtin_displaces_catalog_tool(builtin.as_ref(), language, config, kind) {
            return true;
        }
        warn_catalog_tool_displaced_once(tool.name(), language);
        false
    });
    engines.retain(|engine| !catalog.iter().any(|tool| tool.name() == engine.name()));
    engines.extend(catalog);
    engines
}

/// Drop every registry engine explicitly switched off with `enabled = false`.
///
/// Only an explicit `false` counts. An absent key means "this engine's own
/// default", which differs per backend — `uncomment` is opt-in, `quality` is
/// on, and each `native_tool` spec carries its own `default_on` — so reading
/// absence as `false` would withdraw every engine that never declared the key.
///
/// Applied to the registry engines **before** the catalog merge, not to the
/// merged set, because a built-in and a catalog tool can share a `name()` and
/// therefore share a `[<kind>.<lang>.<name>]` table. `enabled = false` there
/// names the built-in; the catalog tool has its own switch in `[tools.<name>]`.
/// Filtering first is also what lets a switched-off built-in yield to the
/// catalog tool of the same name rather than taking it down too.
fn retaining_enabled(
    engines: Vec<Box<dyn Engine>>,
    language: &Language,
    config: &Config,
    kind: Kind,
    disabled: &mut Option<Disabled>,
) -> Vec<Box<dyn Engine>> {
    engines
        .into_iter()
        .filter(|engine| {
            // A backend that reads the key itself degrades on its own terms —
            // `native_tool` hands its language to the tier-2 reindenter — and
            // dropping it here would skip that fallback entirely, leaving the
            // language with no engine rather than a lower-fidelity one.
            if engine.self_manages_enabled() {
                return true;
            }
            let cfg = config.engine_config(language, engine.name(), kind);
            if cfg.options.get(ENABLED_OPTION_KEY).and_then(toml::Value::as_bool) != Some(false) {
                return true;
            }
            // The same question `selection::apply` asks, for the same reason:
            // "did this withdrawal take away the engines that knew the
            // language", not merely "did it take away an engine".
            let dropped_coverage = kind == Kind::Lint && engine.provides_language_lint(language, &cfg);
            let table = format!("[{}.{}.{}]", kind.section(), language.id(), engine.name());
            match disabled {
                // First one wins the naming, but any of them dropping coverage
                // counts — otherwise disabling two engines would report the
                // first and forget that the second held the rules.
                Some(existing) => existing.dropped_coverage |= dropped_coverage,
                None => {
                    *disabled = Some(Disabled {
                        table,
                        dropped_coverage,
                    })
                }
            }
            false
        })
        .collect()
}

/// What an `enabled = false` removed from one language's plan.
struct Disabled {
    /// The config table that did it, quoted back to the reader.
    table: String,
    /// Whether any engine it removed was one that held lint rules for the
    /// language — the difference between "one fewer check" and "nothing lints
    /// this language any more".
    dropped_coverage: bool,
}

pub(super) fn plan_engines(
    language: &Language,
    config: &Config,
    kind: Kind,
    selection: EngineSelection<'_>,
    withdrawal: &mut Option<Withdrawal>,
) -> Vec<EnginePlan> {
    let mut disabled = None;
    let engines = retaining_capable(engines_for(language), kind);
    let mut engines = retaining_enabled(engines, language, config, kind, &mut disabled);
    // Asked *after* the enabled filter, not before: a disabled tier-one
    // formatter is not a tier-one formatter, and asking first discarded the
    // catalog list on its behalf — so switching off `gofmt` to reach for a
    // catalog formatter produced neither.
    //
    // Ordering alone was not enough. `retaining_enabled` drops only an explicit
    // `enabled = false`, so an opt-in native tool that was never switched on —
    // shfmt, zig fmt, ktfmt, swift-format and the rest default to off — survived
    // it and still counted as tier one. `[tools.shfmt] enabled = true` was
    // therefore discarded here on behalf of a formatter that does nothing, with
    // no diagnostic: `poly fmt` reported "All formatted" over shell it had never
    // given to shfmt. The question is now whether the backend would actually
    // format, which is what the lint side has always asked. ~keep
    let catalog = if has_tier_one_formatter(&engines, language, config, kind) {
        Vec::new()
    } else {
        retaining_capable(catalog_engines_for(language, config, kind), kind)
    };
    if generic_formatter_superseded(kind, &catalog) {
        engines.retain(|engine| engine.name() != TREE_SITTER_ENGINE);
    }
    let merged = merge_catalog_engines(language, config, kind, engines, catalog);
    let (merged, selection_emptied) = selection::apply(selection, merged, language, config, kind);
    // Selection is reported ahead of config: it names this invocation, which is
    // the thing the reader is looking at.
    *withdrawal = if selection_emptied {
        Some(Withdrawal::Selection)
    } else {
        disabled
            .filter(|disabled| disabled.dropped_coverage || merged.is_empty())
            .map(|disabled| Withdrawal::Disabled(disabled.table))
    };
    merged
        .into_iter()
        .map(|engine| {
            let cfg = config.engine_config(language, engine.name(), kind);
            let serialized_args = ResultCache::serialize_args(&cache_args(&cfg));
            let severity_remap = build_severity_remap(&cfg);
            // Only a lint plan can establish lint coverage; asking a formatter
            // would answer a question nobody posed.
            let provides_language_lint = kind == Kind::Lint && engine.provides_language_lint(language, &cfg);
            EnginePlan {
                engine,
                config: cfg,
                serialized_args,
                severity_remap,
                provides_language_lint,
            }
        })
        .collect()
}

/// Whether anything in this plan knows how to lint the language it was built
/// for.
///
/// `false` is the state the run must not report as clean: the file is routed,
/// the cross-cutting backends (spell-check, comment removal) still run over it,
/// but no backend holds a single rule for the language — so a green result says
/// nothing about the code in it.
pub(super) fn provides_language_lint(plans: &[EnginePlan]) -> bool {
    plans.iter().any(|plan| plan.provides_language_lint)
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

/// Whether `key` is being reported for the first time in this process.
///
/// Planning runs once per `(config, language)` pair — several times over in a
/// monorepo with nested configs (ADR 0018) — so a warning raised from it would
/// otherwise repeat for every one of them. Backs the once-per-key warnings below.
fn first_report_of(key: String) -> bool {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static REPORTED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    REPORTED
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .expect("report set poisoned")
        .insert(key)
}

/// Emit a one-time `warn` that an enabled whole-project type-checker is being
/// skipped in the per-file catalog lint tier.
fn warn_whole_project_linter_once(name: &str) {
    if first_report_of(format!("whole-project:{name}")) {
        tracing::warn!(
            tool = name,
            "'{name}' is a whole-project type-checker and cannot run in poly's per-file lint tier; \
             it is skipped. Run it as a dedicated whole-project step instead."
        );
    }
}

/// Emit a one-time `warn` that an enabled `[tools.<name>]` entry is not being
/// run as a catalog tool because poly's own backend of the same name already
/// covers this language.
///
/// Worth saying out loud rather than resolving silently: the tool still runs,
/// but through the built-in wrapper, so any `command` / `args` / `env` the user
/// set on the `[tools.<name>]` table has no effect here.
fn warn_catalog_tool_displaced_once(name: &str, language: &Language) {
    if first_report_of(format!("displaced:{name}:{language:?}")) {
        tracing::warn!(
            tool = name,
            language = language.id(),
            "'{name}' is already built into poly for this language, so the '[tools.{name}]' entry \
             is not run a second time; its 'command'/'args'/'env' settings do not apply. Configure \
             the built-in under '[lint.<language>.{name}]', or disable it there to run the \
             catalog tool instead."
        );
    }
}

/// Emit a one-time `warn` that an enabled `[tools.<name>]` entry cannot run as a
/// catalog linter because the command it would run rewrites files.
///
/// The refusal itself is right — a fix command run as a lint pass overwrites the
/// user's source and still exits zero — but staying quiet about it is its own
/// false pass: the user asked for that linter, poly runs nothing, and the report
/// comes back clean as though the file had been checked. So name the tool, the
/// reason, and the remedy: `poly fmt` still runs these tools.
fn warn_mutating_catalog_linter_once(name: &str, language: &Language, refusal: &LintRefusal) {
    if !first_report_of(format!("mutating-lint:{name}:{language:?}")) {
        return;
    }
    let language_id = language.id();
    match refusal {
        LintRefusal::MutatingCommand(argv) => tracing::warn!(
            tool = name,
            language = language_id,
            "'{name}' is enabled but the lint command it would run rewrites files ('{argv}'), so it \
             cannot run as a linter — it would overwrite your source and still report it clean. It \
             is skipped for {language_id}; 'poly fmt' still runs it as a formatter. To lint with it, \
             point '[tools.{name}]' 'command'/'args' at a check-only command."
        ),
        LintRefusal::AlwaysMutating => tracing::warn!(
            tool = name,
            language = language_id,
            "'{name}' is enabled but rewrites every file it is given and has no check-only mode, so \
             it cannot run as a linter — it would overwrite your source and still report it clean. \
             It is skipped for {language_id}; 'poly fmt' still runs it as a formatter."
        ),
    }
}

/// Build the catalog-driven engines (ADR 0013) for `language`: one
/// [`CatalogToolEngine`] per enabled `[tools.<name>]` whose catalog tool both
/// declares a language that maps to `language` and exposes a usable command for
/// `kind`.
///
/// [`Kind::Format`] wires the tool's format command; [`Kind::Lint`] wires its
/// lint command — but only when that command is **non-mutating**. A command that
/// rewrites the file, whether through a flag (`--fix`, `--autocorrect`,
/// `--in-place`, …) or a subcommand (`sqruff fix`, `ruff format`), would corrupt
/// files if run as a linter, so [`CatalogToolEngine::lint_engine`] skips it.
/// Such a refusal is reported ([`warn_mutating_catalog_linter_once`]) rather than
/// resolved in silence, since from the outside it is indistinguishable from a
/// linter that ran and found nothing. Catalog linting is a best-effort,
/// breadth-tier mechanism (file-level, exit-code based); structured per-tool
/// diagnostics remain the curated native backends' job.
///
/// A tool built here may still be dropped by [`merge_catalog_engines`] when
/// poly's own backend of the same name already covers the language.
fn catalog_engines_for(language: &Language, config: &Config, kind: Kind) -> Vec<Box<dyn Engine>> {
    let catalog = poly_catalog::Catalog::get();
    let mut engines: Vec<Box<dyn Engine>> = Vec::new();
    for (name, tool_config) in config.tools.iter() {
        if !tool_config.enabled {
            continue;
        }
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
        if kind == Kind::Lint && crate::engines::catalog_tool::is_whole_project_linter(name) {
            warn_whole_project_linter_once(name);
            continue;
        }
        let command = tool_config.command.as_deref();
        let args = tool_config.args.as_deref();
        if kind == Kind::Lint
            && let Some(refusal) = crate::engines::catalog_tool::lint_refusal(tool, command, args)
        {
            warn_mutating_catalog_linter_once(name, language, &refusal);
            continue;
        }
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
pub(super) fn prefetch_tier2_grammars(plans: &RunPlan) {
    let grammars: FxHashSet<&str> = plans
        .iter()
        .filter(|(_, engine_plans)| engine_plans.iter().any(|plan| plan.engine.name() == "treesitter"))
        .filter_map(|((_, language), _)| match language {
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
pub(super) fn plan_by_config_language(
    files: &[DiscoveredFile],
    configs: &ConfigSet,
    kind: Kind,
    selection: EngineSelection<'_>,
) -> RunPlan {
    let mut plans: PlanMap = FxHashMap::default();
    let mut withdrawn: WithdrawnMap = FxHashMap::default();
    for f in files {
        let key = (f.config_id, f.language.clone());
        if plans.contains_key(&key) {
            continue;
        }
        let mut withdrawal = None;
        let plan = plan_engines(
            &f.language,
            configs.config(f.config_id),
            kind,
            selection,
            &mut withdrawal,
        );
        if let Some(withdrawal) = withdrawal {
            withdrawn.insert(key.clone(), withdrawal);
        }
        plans.insert(key, plan);
    }
    RunPlan { plans, withdrawn }
}

#[cfg(test)]
mod tests;
