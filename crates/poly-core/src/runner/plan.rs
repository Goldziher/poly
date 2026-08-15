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
use crate::engine::Engine;
use crate::engines::catalog_tool::CatalogToolEngine;
use crate::engines::rule_config::RuleSelection;
use crate::filter::SeverityRemap;
use crate::language::Language;
use crate::registry::engines_for;
use crate::resolve::ConfigSet;

/// A per-file engine plan map, keyed by `(config_id, language)` so a monorepo's
/// nested configs each get their own plans (ADR 0018). In a single-config repo
/// every file shares `config_id == 0`, collapsing to one plan per language.
pub(super) type PlanMap = FxHashMap<(usize, Language), Vec<EnginePlan>>;

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
fn has_tier_one_formatter(engines: &[Box<dyn Engine>], kind: Kind) -> bool {
    kind == Kind::Format
        && engines
            .iter()
            .any(|engine| engine.name() != TREE_SITTER_ENGINE && engine.capabilities().format)
}

pub(super) fn plan_engines(language: &Language, config: &Config, kind: Kind) -> Vec<EnginePlan> {
    let mut engines = engines_for(language);
    let catalog = if has_tier_one_formatter(&engines, kind) {
        Vec::new()
    } else {
        catalog_engines_for(language, config, kind)
    };
    if generic_formatter_superseded(kind, &catalog) {
        engines.retain(|engine| engine.name() != TREE_SITTER_ENGINE);
    }
    engines.extend(catalog);
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
/// Catalog linting is a best-effort, breadth-tier mechanism (file-level,
/// exit-code based); structured per-tool diagnostics remain the curated native
/// backends' job.
/// Emit a one-time `warn` that an enabled whole-project type-checker is being
/// skipped in the per-file catalog lint tier. `catalog_engines_for` runs once per
/// `(config, language)` pair, so the warning is de-duplicated per tool name for
/// the process lifetime to avoid repeating it for every Python file's config.
fn warn_whole_project_linter_once(name: &str) {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static WARNED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let mut warned = WARNED
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .expect("warn set poisoned");
    if warned.insert(name.to_string()) {
        tracing::warn!(
            tool = name,
            "'{name}' is a whole-project type-checker and cannot run in poly's per-file lint tier; \
             it is skipped. Run it as a dedicated whole-project step instead."
        );
    }
}

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
pub(super) fn prefetch_tier2_grammars(plans: &PlanMap) {
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
pub(super) fn plan_by_config_language(files: &[DiscoveredFile], configs: &ConfigSet, kind: Kind) -> PlanMap {
    let mut plans: PlanMap = FxHashMap::default();
    for f in files {
        plans
            .entry((f.config_id, f.language.clone()))
            .or_insert_with(|| plan_engines(&f.language, configs.config(f.config_id), kind));
    }
    plans
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stand-in for a catalog engine, parameterised on whether its binary is present.
    struct StubEngine(bool);

    impl Engine for StubEngine {
        fn name(&self) -> &'static str {
            "stub"
        }

        fn languages(&self) -> &'static [Language] {
            &[]
        }

        fn capabilities(&self) -> crate::engine::Capabilities {
            crate::engine::Capabilities {
                lint: false,
                format: true,
                fix: false,
            }
        }

        fn version(&self) -> &str {
            "0"
        }

        fn supersedes_generic_formatter(&self) -> bool {
            self.0
        }
    }

    fn catalog(available: bool) -> Vec<Box<dyn Engine>> {
        vec![Box::new(StubEngine(available))]
    }

    /// A runnable catalog formatter owns the language, so the generic reindenter
    /// must step aside — otherwise the two chain and fight over indentation.
    #[test]
    fn runnable_catalog_formatter_supersedes_the_generic_one() {
        assert!(generic_formatter_superseded(Kind::Format, &catalog(true)));
    }

    /// A configured-but-missing binary must NOT displace the fallback, or the
    /// language silently loses all formatting.
    #[test]
    fn missing_catalog_binary_leaves_the_generic_formatter_in_place() {
        assert!(!generic_formatter_superseded(Kind::Format, &catalog(false)));
    }

    /// Linting never displaces a formatter.
    #[test]
    fn lint_kind_never_supersedes_the_generic_formatter() {
        assert!(!generic_formatter_superseded(Kind::Lint, &catalog(true)));
    }

    /// No catalog tools configured — the fallback stays.
    #[test]
    fn no_catalog_engines_leaves_the_generic_formatter_in_place() {
        assert!(!generic_formatter_superseded(Kind::Format, &[]));
    }

    #[test]
    fn tier_one_formatter_prevents_catalog_formatter_chaining() {
        let config = Config {
            tools: toml::from_str("[clang-format]\nenabled = true\n").expect("valid tool config"),
            ..Config::default()
        };
        for language in [Language::JavaScript, Language::TypeScript, Language::Jsx, Language::Tsx] {
            let engines = engines_for(&language);
            assert!(
                has_tier_one_formatter(&engines, Kind::Format),
                "{language:?} must remain owned by its tier-one formatter"
            );
            let plan = plan_engines(&language, &config, Kind::Format);
            assert!(plan.iter().any(|entry| entry.engine.name() == "oxc"));
            assert!(!plan.iter().any(|entry| entry.engine.name() == "clang-format"));
        }
    }

    /// The routing fact behind the whole defect: a `.kt` lint plan is built
    /// entirely from cross-cutting backends, so nothing in it holds a Kotlin
    /// rule. Pinned here because it is invisible from the outside — the plan is
    /// non-empty, the engines run, and the file was counted as linted on that
    /// basis alone.
    #[test]
    fn a_language_with_no_backend_of_its_own_has_no_lint_coverage() {
        let config = Config::default();
        for language in [Language::Kotlin, Language::Swift, Language::Zig, Language::Rust] {
            let plan = plan_engines(&language, &config, Kind::Lint);
            assert!(
                !plan.is_empty(),
                "{language:?} is still routed to the cross-cutting backends"
            );
            assert!(
                !provides_language_lint(&plan),
                "{language:?} has no lint rules and must not claim coverage"
            );
        }
    }

    /// The other side of the same check: a language with a native backend does
    /// carry coverage, so the common path gains nothing and reports nothing.
    #[test]
    fn a_language_with_a_native_backend_has_lint_coverage() {
        let config = Config::default();
        for language in [Language::Python, Language::Toml, Language::Yaml, Language::Markdown] {
            assert!(
                provides_language_lint(&plan_engines(&language, &config, Kind::Lint)),
                "{language:?} is linted by its own backend"
            );
        }
    }

    /// Coverage is a lint question. A format plan is not asked it, so a
    /// formatter can never be mistaken for evidence that a file was linted.
    #[test]
    fn a_format_plan_never_claims_lint_coverage() {
        let config = Config::default();
        assert!(!provides_language_lint(&plan_engines(
            &Language::Python,
            &config,
            Kind::Format
        )));
    }

    #[test]
    fn generic_language_allows_catalog_formatter() {
        let config = Config {
            tools: toml::from_str("[clang-format]\nenabled = true\n").expect("valid tool config"),
            ..Config::default()
        };
        let plan = plan_engines(&Language::C, &config, Kind::Format);
        assert!(plan.iter().any(|entry| entry.engine.name() == "clang-format"));
    }
}
