//! Tests for the per-language engine plan.
//!
//! Split into its own file because `plan.rs` is at the repository's 1000-line
//! module cap; a path named `tests.rs` is exempt from it.

use super::*;
use crate::engines::catalog_tool::CATALOG_VERSION_PREFIX;

/// The selection every test that is not about `--only` wants: none.
fn unrestricted() -> EngineSelection<'static> {
    EngineSelection::new(&[], &[])
}

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
        let plan = plan_engines(&language, &config, Kind::Format, unrestricted(), &mut false);
        assert!(plan.iter().any(|entry| entry.engine.name() == "oxc"));
        assert!(!plan.iter().any(|entry| entry.engine.name() == "clang-format"));
    }
}

/// The engines in `plan` that claim to hold lint rules for the language it
/// was built for — the set `provides_language_lint` reduces to a boolean,
/// asserted by name so a test says *which* backend establishes coverage
/// rather than only that something did.
fn covering_engines(plan: &[EnginePlan]) -> Vec<&'static str> {
    plan.iter()
        .filter(|entry| entry.provides_language_lint)
        .map(|entry| entry.engine.name())
        .collect()
}

/// The routing fact behind the original defect, in the half that still
/// holds: a `.zig` or `.dart` lint plan is built entirely from
/// cross-cutting backends, and neither of the two that read structure
/// contributes language knowledge — `quality` has no construct table for
/// either grammar, and the built-in ast-grep pack ships no rule for them —
/// so all they leave is a line count and an ignore-marker scan, which is
/// exactly what a `.txt` file gets. Counting lines is not knowledge of the
/// language, so the file must not be counted as linted. Pinned here
/// because it is invisible from the outside: the plan is non-empty and the
/// engines all run.
///
/// Swift used to sit in this list and no longer can: the pack's
/// `force-cast` / `force-try` are genuine Swift rules, so its coverage is
/// now real. That is the expected direction of travel — a language leaves
/// this test when poly learns something about it, never the reverse.
#[test]
fn a_language_the_cross_cutting_tier_only_line_counts_has_no_lint_coverage() {
    let config = Config::default();
    for language in [Language::Zig, Language::Dart] {
        let plan = plan_engines(&language, &config, Kind::Lint, unrestricted(), &mut false);
        assert!(
            !plan.is_empty(),
            "{language:?} is still routed to the cross-cutting backends"
        );
        assert_eq!(
            covering_engines(&plan),
            Vec::<&str>::new(),
            "{language:?} has no lint rules and must not claim coverage"
        );
    }
}

/// The other half, and the point of ADR 0027: C and C++ have no lint
/// backend of their own and no built-in ast-grep rule, but the `quality`
/// tier holds a genuine structural model of both grammars — a definition
/// query *and* a construct table — so function size, nesting and
/// complexity really are measured and the run is right to count the file.
/// Asserted by engine name, since `quality` is the only thing standing
/// between these two languages and a skip; if a future pack rule covers
/// them, move them out rather than relaxing the assertion, or a regression
/// that drops `quality`'s coverage would hide behind the other engine.
#[test]
fn a_language_the_quality_tier_structurally_models_has_lint_coverage() {
    let config = Config::default();
    for language in [Language::C, Language::Cpp] {
        let plan = plan_engines(&language, &config, Kind::Lint, unrestricted(), &mut false);
        assert_eq!(
            covering_engines(&plan),
            vec!["quality"],
            "{language:?} is linted by the quality tier alone"
        );
    }
}

/// Rust and Kotlin are covered twice over, and by two different kinds of
/// knowledge: the built-in ast-grep pack holds hand-written rules for both
/// (`unwrap-used`, `not-null-assertion`, …) while `quality` measures their
/// structure. Asserted as the exact pair so that losing either one — a
/// pack rule set emptied, or a construct table dropped — fails here
/// instead of silently halving the coverage behind a still-true boolean.
#[test]
fn a_language_both_the_pack_and_the_quality_tier_cover_lists_both() {
    let config = Config::default();
    for language in [Language::Rust, Language::Kotlin] {
        let plan = plan_engines(&language, &config, Kind::Lint, unrestricted(), &mut false);
        assert_eq!(
            covering_engines(&plan),
            vec!["astgrep", "quality"],
            "{language:?} is linted by the built-in pack and the quality tier"
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
            provides_language_lint(&plan_engines(
                &language,
                &config,
                Kind::Lint,
                unrestricted(),
                &mut false
            )),
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
        Kind::Format,
        unrestricted(),
        &mut false
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
            for plan in plan_engines(&language, &config, kind, unrestricted(), &mut false) {
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
    let plan = plan_engines(&Language::C, &config, Kind::Format, unrestricted(), &mut false);
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
    let plan = plan_engines(&Language::Python, &config, Kind::Lint, unrestricted(), &mut false);
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
    let plan = plan_engines(&Language::Shell, &config, Kind::Lint, unrestricted(), &mut false);
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
        provides_language_lint(&plan_engines(
            &Language::Shell,
            &config,
            Kind::Lint,
            unrestricted(),
            &mut false
        )),
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

/// Build a `Config` whose `[lint]` section is `toml`.
fn lint_config(toml_src: &str) -> Config {
    Config {
        lint: toml::from_str(toml_src).expect("valid lint config"),
        ..Config::default()
    }
}

fn planned_names(plan: &[EnginePlan]) -> Vec<&'static str> {
    plan.iter().map(|entry| entry.engine.name()).collect()
}

/// The universal `enabled` key disables *any* engine, including a tier-one
/// backend that never declared the key for itself. Before this existed,
/// `[lint.python.ruff] enabled = false` was reported as an unknown key and
/// ruff ran anyway — a setting that reads as honoured and does nothing.
#[test]
fn a_universally_disabled_engine_is_dropped_from_the_plan() {
    let config = lint_config("[python.ruff]\nenabled = false\n");
    let plan = plan_engines(&Language::Python, &config, Kind::Lint, unrestricted(), &mut false);
    assert!(
        !planned_names(&plan).contains(&"ruff"),
        "ruff must not be planned once it is disabled, got {:?}",
        planned_names(&plan)
    );
}

/// The same key set to `true` is a no-op on an engine that was already on:
/// the flag narrows, it never re-orders the plan.
#[test]
fn an_explicitly_enabled_engine_stays_planned() {
    let config = lint_config("[python.ruff]\nenabled = true\n");
    assert!(
        planned_names(&plan_engines(
            &Language::Python,
            &config,
            Kind::Lint,
            unrestricted(),
            &mut false
        ))
        .contains(&"ruff")
    );
}

/// Absent means "the engine's own default", never "off" — otherwise every
/// engine that does not declare the key would vanish from every plan.
#[test]
fn an_absent_enabled_key_leaves_the_plan_untouched() {
    let config = Config::default();
    assert!(
        planned_names(&plan_engines(
            &Language::Python,
            &config,
            Kind::Lint,
            unrestricted(),
            &mut false
        ))
        .contains(&"ruff")
    );
}

/// A cross-cutting backend is disabled from its language-agnostic table,
/// which is the only place a user can name it globally.
#[test]
fn a_cross_cutting_engine_is_disabled_from_its_language_agnostic_table() {
    let config = lint_config("[typos]\nenabled = false\n");
    for language in [Language::Python, Language::Rust, Language::Toml] {
        let names = planned_names(&plan_engines(
            &language,
            &config,
            Kind::Lint,
            unrestricted(),
            &mut false,
        ));
        assert!(!names.contains(&"typos"), "{language:?} still plans typos: {names:?}");
    }
}

/// ...and from a per-language table, which overrides the global one. This
/// is the case `build_astgrep_options` could not express: it never reads
/// `lang_options` at all, so the key is carried by `engine_config` itself
/// rather than by each cross-cutting builder.
#[test]
fn a_cross_cutting_engine_is_disabled_per_language() {
    let config = lint_config("[rust.astgrep]\nenabled = false\n");
    let rust = planned_names(&plan_engines(
        &Language::Rust,
        &config,
        Kind::Lint,
        unrestricted(),
        &mut false,
    ));
    assert!(!rust.contains(&"astgrep"), "rust still plans astgrep: {rust:?}");
    let python = planned_names(&plan_engines(
        &Language::Python,
        &config,
        Kind::Lint,
        unrestricted(),
        &mut false,
    ));
    assert!(
        python.contains(&"astgrep"),
        "another language must be unaffected: {python:?}"
    );
}

/// An opt-in engine keeps its own default when the key is absent: the
/// universal check must not promote "not configured" into "off" and drop
/// `uncomment` from the plan before it can read its own setting.
#[test]
fn an_opt_in_engine_is_still_planned_when_the_key_is_absent() {
    let config = Config::default();
    assert!(
        planned_names(&plan_engines(
            &Language::Python,
            &config,
            Kind::Lint,
            unrestricted(),
            &mut false
        ))
        .contains(&"uncomment")
    );
}

/// Disabling the only backend that holds rules for a language withdraws
/// the language's lint coverage too — the run must not keep claiming it
/// linted Python once ruff is off.
#[test]
fn disabling_the_language_backend_withdraws_its_lint_coverage() {
    let config = lint_config("[python.ruff]\nenabled = false\n");
    let plan = plan_engines(&Language::Python, &config, Kind::Lint, unrestricted(), &mut false);
    assert!(
        !covering_engines(&plan).contains(&"ruff"),
        "a disabled engine must not claim coverage"
    );
}
