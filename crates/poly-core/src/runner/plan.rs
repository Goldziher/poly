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
/// Formatting never reaches this question with a real contender:
/// [`has_tier_one_formatter`] already yields the whole catalog to a registry
/// formatter, so a `Kind::Format` collision can only involve the generic tier,
/// which the built-in rightly owns.
fn builtin_displaces_catalog_tool(builtin: &dyn Engine, language: &Language, config: &Config, kind: Kind) -> bool {
    match kind {
        Kind::Format => true,
        // Costs a `PATH` probe or a rule-pack load, but `plan_engines` runs once
        // per (config, language) — never per file — so it stays out of the hot loop.
        Kind::Lint => {
            let cfg = config.engine_config(language, builtin.name(), kind);
            builtin.provides_language_lint(language, &cfg)
        }
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
    let engines = retaining_capable(engines, kind);
    let catalog = retaining_capable(catalog, kind);
    merge_catalog_engines(language, config, kind, engines, catalog)
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
    use crate::engines::catalog_tool::CATALOG_VERSION_PREFIX;

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

    /// A [`Config`] with every catalog tool switched on — the widest plan any
    /// user config can produce, so a name collision that is reachable at all is
    /// reachable here.
    fn every_catalog_tool_enabled() -> Config {
        let mut source = String::new();
        for tool in poly_catalog::Catalog::get().tools() {
            source.push_str(&format!("[\"{}\"]\nenabled = true\n", tool.name));
        }
        Config {
            tools: toml::from_str(&source).expect("catalog names are valid tool config keys"),
            ..Config::default()
        }
    }

    /// Every language a plan can be built for: the registry's own exhaustive
    /// list plus every language any catalog tool claims, so the walk covers the
    /// [`Language::Other`] arms a catalog-only language lands in.
    fn every_plannable_language() -> Vec<Language> {
        let mut languages = crate::registry::tests::all_known_languages();
        for tool in poly_catalog::Catalog::get().tools() {
            for name in &tool.languages {
                let language = Language::from_catalog_name(name);
                if !languages.contains(&language) {
                    languages.push(language);
                }
            }
        }
        languages
    }

    /// The uniqueness guard, at the level that actually matters.
    ///
    /// `registry::tests::registered_engine_names_are_unique_per_language` walks
    /// `engines_for` alone, but [`plan_engines`] appends the catalog engines to
    /// that same `Vec` — same capability filter, same
    /// `config.engine_config(language, engine.name(), kind)` lookup, same
    /// `ResultCache` args. Two plans sharing a name therefore share one config
    /// table, one severity remap, and one `Diagnostic::engine` label, and a
    /// reader of `--format json` cannot tell which of the two produced a finding.
    /// Checking only the registry left that reachable with documented config
    /// (`[tools.shellcheck]` + `[lint.shell.shellcheck]` put two `"shellcheck"`
    /// engines in one Shell lint plan, reporting every finding twice).
    #[test]
    fn planned_engine_names_are_unique_per_language_and_kind() {
        let config = every_catalog_tool_enabled();
        let mut collisions: Vec<String> = Vec::new();
        for language in every_plannable_language() {
            for kind in [Kind::Lint, Kind::Format] {
                let mut seen: Vec<&'static str> = Vec::new();
                for plan in plan_engines(&language, &config, kind) {
                    let name = plan.engine.name();
                    if seen.contains(&name) {
                        collisions.push(format!("{kind:?} plan for {language:?}: {name:?}"));
                    }
                    seen.push(name);
                }
            }
        }
        assert!(
            collisions.is_empty(),
            "these plans place two engines under one name, so they share a \
             [<kind>.<lang>.<name>] config table, a severity remap, and a diagnostic label, \
             and report the same finding twice:\n  {}",
            collisions.join("\n  "),
        );
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

    /// Build a config from a literal `poly.toml` body, so these tests exercise
    /// the same parse path a user's file takes.
    fn config_from(source: &str) -> Config {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("poly.toml");
        std::fs::write(&path, source).expect("write poly.toml");
        Config::load_file(&path).expect("load poly.toml")
    }

    /// The engines in `plan` answering `name`, so a test can assert both how
    /// many there are and which tier each came from.
    fn planned_versions(plan: &[EnginePlan], name: &str) -> Vec<String> {
        plan.iter()
            .filter(|entry| entry.engine.name() == name)
            .map(|entry| entry.engine.version().to_owned())
            .collect()
    }

    /// The collision the guard above now forbids, resolved: a `[tools.ruff]`
    /// entry alongside poly's own `ruff` backend leaves **one** `ruff` in the
    /// Python lint plan, and it is the built-in — the higher-fidelity wrapper,
    /// which reports spans and rule codes where the catalog tier reports one
    /// file-level pass/fail. The argv override is what makes the catalog lint
    /// engine buildable at all: the catalog's own `check` command carries
    /// `--fix`, which `lint_engine` rejects as mutating.
    #[test]
    fn an_active_builtin_displaces_the_catalog_tool_of_the_same_name() {
        let config = config_from("[tools.ruff]\nenabled = true\nargs = [\"check\", \"--quiet\", \"$PATH\"]\n");
        let plan = plan_engines(&Language::Python, &config, Kind::Lint);
        let versions = planned_versions(&plan, "ruff");
        assert_eq!(
            versions.len(),
            1,
            "one tool, one engine: got {versions:?} — a second would report every finding twice",
        );
        assert!(
            !versions[0].starts_with(CATALOG_VERSION_PREFIX),
            "the built-in ruff must be the survivor, got the catalog engine: {versions:?}",
        );
    }

    /// The other half of the rule, and the reason it is not a blanket "built-in
    /// always wins": `shellcheck` is opt-in, so with `[lint.shell.shellcheck]`
    /// off the built-in engine lints nothing. Letting it displace the catalog
    /// tool would turn a duplicated diagnostic into no diagnostic at all, so the
    /// catalog engine survives instead and the inert built-in is the one dropped.
    #[test]
    fn an_inactive_builtin_yields_to_the_catalog_tool_of_the_same_name() {
        let config = config_from("[tools.shellcheck]\nenabled = true\n\n[lint.shell.shellcheck]\nenabled = false\n");
        let plan = plan_engines(&Language::Shell, &config, Kind::Lint);
        let versions = planned_versions(&plan, "shellcheck");
        assert_eq!(
            versions.len(),
            1,
            "expected exactly one shellcheck engine, got {versions:?}"
        );
        assert!(
            versions[0].starts_with(CATALOG_VERSION_PREFIX),
            "the catalog engine must survive when the built-in is switched off, got {versions:?}",
        );
    }

    /// Displacing an engine must never cost lint coverage. With the built-in
    /// `shellcheck` switched off, the surviving catalog engine is what keeps the
    /// Shell plan claiming coverage — the property `provides_language_lint`
    /// exists to report honestly.
    #[test]
    fn a_surviving_catalog_tool_still_carries_lint_coverage() {
        if which::which("shellcheck").is_err() {
            // Coverage is claimed only for a binary that is actually on PATH,
            // so with none installed there is nothing to assert here.
            return;
        }
        let config = config_from("[tools.shellcheck]\nenabled = true\n\n[lint.shell.shellcheck]\nenabled = false\n");
        assert!(
            provides_language_lint(&plan_engines(&Language::Shell, &config, Kind::Lint)),
            "the catalog shellcheck engine must still establish Shell lint coverage",
        );
    }

    /// The `catalog:` version prefix is load-bearing, not decorative.
    ///
    /// A cache key is `(namespace, engine name, engine version, args, digest)`.
    /// A catalog tool and a registry backend can answer the same name and, for a
    /// given file, the same args and digest — so `version()` is the only field
    /// left to separate them, and it separates them only because every catalog
    /// engine's version starts with a prefix no registry backend's version uses.
    /// That was true by accident; this pins it, in both directions.
    #[test]
    fn catalog_and_builtin_cache_key_spaces_are_disjoint() {
        for language in every_plannable_language() {
            for engine in engines_for(&language) {
                assert!(
                    !engine.version().starts_with(CATALOG_VERSION_PREFIX),
                    "registry backend {:?} reports a version starting with {CATALOG_VERSION_PREFIX:?} \
                     ({:?}); that prefix is what keeps catalog results from being served to a \
                     built-in engine under a shared name",
                    engine.name(),
                    engine.version(),
                );
            }
        }

        let config = every_catalog_tool_enabled();
        let mut catalog_engines_seen = 0_usize;
        for language in every_plannable_language() {
            for kind in [Kind::Lint, Kind::Format] {
                for engine in catalog_engines_for(&language, &config, kind) {
                    catalog_engines_seen += 1;
                    assert!(
                        engine.version().starts_with(CATALOG_VERSION_PREFIX),
                        "catalog engine {:?} must stamp its version with {CATALOG_VERSION_PREFIX:?}, got {:?}",
                        engine.name(),
                        engine.version(),
                    );
                }
            }
        }
        assert!(
            catalog_engines_seen > 0,
            "built zero catalog engines; the traversal is broken, not the invariant",
        );
    }
}
