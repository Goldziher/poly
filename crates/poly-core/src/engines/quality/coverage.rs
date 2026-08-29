//! Which languages `quality` may claim lint coverage for
//! ([`crate::engine::Engine::provides_language_lint`]).
//!
//! `quality` is registered for every language (`languages() == &[]`), but
//! being *routed* a file is not the same as holding a rule that knows its
//! language — the distinction `runner::lint_one` reports as
//! `no lint rules for <language>`, counts in the run's `checked` total, and
//! `--deny-skips` fails on. Answering that question from the engine's
//! `enabled` flag alone would claim every file in every repository as linted
//! and delete the skip entirely, which is the overclaim this module exists to
//! prevent. It follows the rule `engines/astgrep/mod.rs` already records:
//! coverage is answered by the *same per-language lookup* [`Engine::lint`](crate::engine::Engine::lint)
//! performs, never by the weaker "the engine is switched on".
//!
//! # What counts as knowing a language
//!
//! Two of `quality`'s rules are not language knowledge at all:
//!
//! - `file-too-long` counts newlines,
//! - `lazy-ignore` scans for ignore markers as text.
//!
//! Both run over a `.zig` file exactly as they run over a `.txt` file, and
//! neither needs a parser. `magic-number` and `law-of-demeter` do walk a
//! parse tree, but by their own module docs they are deliberately
//! *grammar-generic* substring heuristics over node-kind names — a shape
//! accepted there precisely because it is not a per-language model — and both
//! are opt-in and off by default.
//!
//! That leaves the five default-on structural rules, which need one of two
//! per-grammar tables:
//!
//! | Rules | Model required |
//! |---|---|
//! | `function-too-long`, `type-too-long`, `too-many-parameters` | [`definitions::has_query`] — a Tags (or built-in) query naming the grammar's definitions |
//! | `nesting-too-deep`, `cyclomatic-complexity` | [`kinds::has_table`] — the hand-verified construct table |
//!
//! [`has_structural_model`] requires **both**, and derives the answer from
//! those two functions rather than from a list of language names, so the
//! covered set cannot drift away from what [`Engine::lint`](crate::engine::Engine::lint) actually does.
//! Verified against the pinned `tree-sitter-language-pack` (see the tests
//! below), that resolves to exactly: Python, Rust, Go, JavaScript (and JSX,
//! which parses as JavaScript), TypeScript, TSX, Java, Kotlin, C, C++, C#,
//! and Ruby.
//!
//! # Why a definition query alone is not enough
//!
//! Zig, Swift, Dart, Gleam, Elixir, PHP, Nix, Scala, Lua and R resolve a
//! definition query but have no construct table. Their functions can be
//! *measured* (how many lines, how many parameters) while everything that
//! happens inside them — every branch, loop and `switch` — is invisible:
//! `nesting-too-deep` and `cyclomatic-complexity` simply do not run. Counting
//! such a file as linted would assert control-flow knowledge poly does not
//! have, so those languages keep the honest `no lint rules for <language>`
//! skip. The findings their partial rules do produce are still reported —
//! exactly like a `typos` or `uncomment` finding on an unlinted language,
//! which the runner has never treated as coverage.

use super::family::{self, Rule};
use super::settings::Settings;
use super::{definitions, kinds};
use crate::language::Language;

/// The grammar `quality`'s structural rules run under for `language`.
///
/// Shares the fallback arm of [`super::grammar_name`], which prefers the
/// language pack's own path-based detection. The one language whose
/// [`Language::id`] is not a grammar name is JSX: the pack has no `jsx`
/// grammar and resolves `*.jsx` to `javascript` (pinned by a test below), so
/// answering with `"jsx"` here would deny coverage to files the engine
/// structurally analyses as JavaScript.
pub fn grammar_for(language: &Language) -> &str {
    match language {
        Language::Jsx => "javascript",
        other => other.id(),
    }
}

/// Whether `grammar` has both halves of the structural model: a definition
/// query *and* a construct table. See the module docs for why one alone does
/// not establish coverage.
pub fn has_structural_model(grammar: &str) -> bool {
    definitions::has_query(grammar) && kinds::has_table(grammar)
}

/// The five structural rules paired with their resolved `enabled` flag, so
/// the coverage answer is computed from the same [`Settings`] and the same
/// [`family::is_deferred`] table [`Engine::lint`](crate::engine::Engine::lint) consults before running each
/// one.
fn structural_rules(settings: &Settings) -> [(Rule, bool); 5] {
    [
        (Rule::FunctionTooLong, settings.function_too_long.enabled),
        (Rule::TypeTooLong, settings.type_too_long.enabled),
        (Rule::TooManyParameters, settings.too_many_parameters.enabled),
        (Rule::NestingTooDeep, settings.nesting_too_deep.enabled),
        (Rule::CyclomaticComplexity, settings.cyclomatic_complexity.enabled),
    ]
}

/// Whether `quality` genuinely lints `language`: the engine is on, the
/// grammar has a full structural model, and at least one structural rule is
/// both enabled and not deferred to a tier-1 backend.
///
/// The last clause is what keeps the deferral table honest. A language whose
/// every structural rule is deferred contributes nothing but the
/// language-agnostic floor, whatever its grammar supports — no language is in
/// that position today (PHP defers three of the five and has no construct
/// table anyway; Python defers two, JS/TS one), but deriving the answer means
/// a future table change flips this rather than silently overclaiming.
pub fn provides_language_lint(language: &Language, settings: &Settings) -> bool {
    settings.enabled
        && has_structural_model(grammar_for(language))
        && structural_rules(settings)
            .iter()
            .any(|(rule, enabled)| *enabled && !family::is_deferred(*rule, language))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{EngineConfig, GlobalDefaults};

    /// Languages whose grammar the module docs claim a full structural model
    /// for. Every one is *asserted* against the two tables below rather than
    /// trusted, so this stays a restatement of the code and never becomes a
    /// second source of truth.
    const MODELLED: &[Language] = &[
        Language::Python,
        Language::Rust,
        Language::Go,
        Language::JavaScript,
        Language::Jsx,
        Language::TypeScript,
        Language::Tsx,
        Language::Java,
        Language::Kotlin,
        Language::C,
        Language::Cpp,
        Language::CSharp,
        Language::Ruby,
    ];

    /// Languages that reach only the language-agnostic floor
    /// (`file-too-long` + `lazy-ignore`).
    const LINE_COUNTED_ONLY: &[Language] = &[
        Language::Zig,
        Language::Swift,
        Language::Dart,
        Language::Gleam,
        Language::Elixir,
        Language::Nix,
        Language::Shell,
        Language::Markdown,
        Language::Dockerfile,
        Language::Hcl,
    ];

    fn settings(options: toml::Table) -> Settings {
        Settings::from_config(&EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 4,
            options,
        })
    }

    fn defaults() -> Settings {
        settings(toml::Table::new())
    }

    /// Fails if the predicate ever reverts to always-false: these are the
    /// languages ADR 0027 gives a lint floor to for the first time, and the
    /// win is worthless if the run does not count them.
    #[test]
    fn a_language_with_a_full_structural_model_claims_coverage() {
        let settings = defaults();
        for language in MODELLED {
            assert!(
                provides_language_lint(language, &settings),
                "{language:?} has both a definition query and a construct table, so quality lints it"
            );
        }
    }

    /// Fails if the predicate ever reverts to always-true (the defect this
    /// module fixes): counting lines and scanning for ignore markers is what
    /// a plain text file gets, and it is not knowledge of the language.
    #[test]
    fn a_language_that_only_gets_line_counting_claims_no_coverage() {
        let settings = defaults();
        for language in LINE_COUNTED_ONLY {
            assert!(
                !provides_language_lint(language, &settings),
                "{language:?} reaches only file-too-long + lazy-ignore, which is not language knowledge"
            );
        }
    }

    /// The covered set is exactly the intersection of the two per-grammar
    /// tables — asserted against the tables themselves, so adding a grammar
    /// to either one is all it takes to move a language.
    #[test]
    fn the_modelled_set_is_exactly_the_grammars_holding_both_tables() {
        for language in MODELLED {
            let grammar = grammar_for(language);
            assert!(
                definitions::has_query(grammar),
                "{grammar} must have a definition query"
            );
            assert!(kinds::has_table(grammar), "{grammar} must have a construct table");
        }
        for language in LINE_COUNTED_ONLY {
            assert!(
                !has_structural_model(grammar_for(language)),
                "{language:?} must not resolve a full structural model"
            );
        }
    }

    /// The half-modelled case, spelled out: these grammars *do* resolve a
    /// definition query, and are still excluded because nesting and
    /// complexity cannot run for them. Pinned so the conjunction in
    /// [`has_structural_model`] cannot be relaxed to an `||` unnoticed.
    #[test]
    fn a_definition_query_without_a_construct_table_is_not_a_model() {
        for grammar in [
            "zig", "swift", "dart", "gleam", "elixir", "php", "nix", "scala", "lua", "r",
        ] {
            assert!(
                definitions::has_query(grammar),
                "{grammar} is expected to have a definition query"
            );
            assert!(
                !kinds::has_table(grammar),
                "{grammar} is expected to have no construct table"
            );
            assert!(
                !has_structural_model(grammar),
                "{grammar} measures function length but sees no control flow, so it is not modelled"
            );
        }
    }

    /// The predicate must name the grammar the lint path will actually parse
    /// with, or coverage is claimed for one grammar and computed from
    /// another. `*.jsx` is the case that makes this more than a tautology:
    /// there is no `jsx` grammar at all.
    #[test]
    fn the_claimed_grammar_is_the_one_the_lint_path_resolves() {
        let paths: &[(Language, &str)] = &[
            (Language::Python, "f.py"),
            (Language::Rust, "f.rs"),
            (Language::Go, "f.go"),
            (Language::JavaScript, "f.js"),
            (Language::Jsx, "f.jsx"),
            (Language::TypeScript, "f.ts"),
            (Language::Tsx, "f.tsx"),
            (Language::Java, "f.java"),
            (Language::Kotlin, "f.kt"),
            (Language::C, "f.c"),
            (Language::Cpp, "f.cpp"),
            (Language::CSharp, "f.cs"),
            (Language::Ruby, "f.rb"),
        ];
        for (language, path) in paths {
            assert_eq!(
                tree_sitter_language_pack::detect_language(path),
                Some(grammar_for(language)),
                "{language:?}: the coverage answer and the parsed grammar must agree",
            );
        }
    }

    #[test]
    fn a_disabled_engine_claims_no_coverage() {
        let mut options = toml::Table::new();
        options.insert("enabled".to_owned(), toml::Value::Boolean(false));
        assert!(!provides_language_lint(&Language::Rust, &settings(options)));
    }

    /// A modelled language whose every non-deferred structural rule is
    /// switched off is back to the floor. Python is the sharpest case:
    /// `too-many-parameters` and `cyclomatic-complexity` are already deferred
    /// to ruff, so turning off the remaining three leaves nothing.
    #[test]
    fn a_language_with_every_live_structural_rule_off_claims_no_coverage() {
        let mut options = toml::Table::new();
        for key in ["function_too_long", "type_too_long", "nesting_too_deep"] {
            options.insert(key.to_owned(), toml::Value::Boolean(false));
        }
        let settings = settings(options);
        assert!(
            !provides_language_lint(&Language::Python, &settings),
            "Python's other two structural rules are deferred to ruff, so nothing structural is left"
        );
        assert!(
            provides_language_lint(&Language::Go, &settings),
            "Go defers nothing, so too-many-parameters and cyclomatic-complexity still run"
        );
    }
}
