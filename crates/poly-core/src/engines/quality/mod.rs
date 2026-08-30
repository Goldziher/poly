//! `quality`: cross-cutting code-quality metric engine (ADR 0027, phase 3a).
//!
//! Modelled on [`crate::engines::uncomment`]: `languages() == &[]`, so the
//! registry appends it to every language, and each rule's own on/off state
//! (plus the per-language deferral table in [`family`]) decides what
//! actually fires — not the registry.
//!
//! # Rules
//!
//! | Code | Default | On by default? |
//! |---|---|---|
//! | `file-too-long` | 1000 lines | yes |
//! | `function-too-long` | 80 lines | yes |
//! | `type-too-long` | 300 lines | yes |
//! | `too-many-parameters` | 6 | yes (deferred for Python, PHP) |
//! | `nesting-too-deep` | 4 | yes (deferred for JS/TS, PHP) |
//! | `cyclomatic-complexity` | 20 | yes (deferred for Python, PHP) |
//! | `lazy-ignore` | — | yes (Rust `#[allow(..)]` deferred to the pack) |
//! | `magic-number` | allow `-1,0,1,2,10,100` | **no** (opt-in) |
//! | `law-of-demeter` | depth 3 | **no** (opt-in) |
//!
//! See [`family`] for the full deferral-table derivation.
//!
//! # Honest degradation
//!
//! `file-too-long` and `lazy-ignore` are pure text scans and work for every
//! language. The structural rules (`function-too-long`, `type-too-long`,
//! `too-many-parameters`, `nesting-too-deep`, `cyclomatic-complexity`,
//! `magic-number`, `law-of-demeter`) need a parse tree: they run only for a
//! grammar `tree-sitter-language-pack` can load, and the two per-grammar
//! rule families degrade independently and honestly when their own table
//! has no entry for a grammar, rather than guessing:
//!
//! - `function-too-long`/`type-too-long`/`too-many-parameters` need
//!   [`definitions::has_query`] — a Tags (or built-in) query for the
//!   grammar. See `definitions` module docs for exactly which languages
//!   that covers and the quirks handled per language.
//! - `nesting-too-deep`/`cyclomatic-complexity` need
//!   [`kinds::has_table`] — a node-kind construct table, verified for
//!   Python, Rust, Go, JavaScript, TypeScript, TSX, Java, Kotlin, C, C++,
//!   C#, and Ruby. Every other grammar simply does not get these two rules
//!   rather than risk a silently wrong structural number (the C
//!   `function_declarator` trap this tier must avoid).
//!
//! Degrading honestly at the rule level also has to be reported honestly at
//! the run level: a language left with nothing but the line count and the
//! marker scan is not a language this engine lints, and
//! [`Engine::provides_language_lint`] says so. See [`coverage`].

pub mod complexity;
pub mod coverage;
pub mod definitions;
pub mod demeter;
pub mod family;
pub mod kinds;
pub mod lazy_ignore;
pub mod magic_number;
mod metrics;
pub mod nesting;
pub mod settings;

use std::cell::RefCell;
use std::collections::HashMap;

use tree_sitter::Parser;
use tree_sitter_language_pack::detect_language;

use crate::config::EngineConfig;
use crate::engine::{Capabilities, Diagnostic, Engine, OptionKeys, OptionTable, SourceFile};
use crate::language::Language;
use family::Rule;
use settings::Settings;

/// Cache-key version. Bump whenever `tree-sitter-language-pack` is upgraded
/// (grammars can change) or this engine's own detection/threshold logic
/// changes in a way that alters output.
const QUALITY_VERSION: &str = "quality-2+tslp1.15.7+no-rust-allow";

thread_local! {
    /// Per-thread parser pool keyed by grammar name, shared by every
    /// structural sub-rule (definitions, nesting, complexity, magic-number,
    /// law-of-demeter) so a file is parsed exactly once per lint pass rather
    /// than once per rule.
    static PARSERS: RefCell<HashMap<String, Parser>> = RefCell::new(HashMap::new());
}

/// Cross-cutting code-quality metric engine. See the module docs.
pub struct QualityEngine;

impl Engine for QualityEngine {
    fn name(&self) -> &'static str {
        "quality"
    }

    fn languages(&self) -> &'static [Language] {
        &[]
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            lint: true,
            format: false,
            fix: false,
        }
    }

    /// The per-rule toggles and thresholds, taken from the one list
    /// `Config::build_quality_options` merges (see
    /// [`settings::BOOL_OPTION_KEYS`]). The same set applies to the
    /// language-agnostic `[lint.quality]` table and to the per-language
    /// `[lint.<lang>.quality]` override.
    fn option_keys(&self, table: OptionTable) -> OptionKeys {
        match table {
            OptionTable::Lint | OptionTable::CrossCuttingLint => OptionKeys::declared(&settings::OPTION_KEYS),
            OptionTable::Format => OptionKeys::declared(&[]),
        }
    }

    fn version(&self) -> &str {
        QUALITY_VERSION
    }

    /// Cross-cutting by registration, but a language is only *linted* here
    /// when this engine holds a structural model of it — answered by
    /// [`coverage::provides_language_lint`] from the same per-grammar tables,
    /// deferral table and [`Settings`] [`Engine::lint`] consults below.
    ///
    /// The weaker "the engine is enabled" answer would claim every file in
    /// every repository, since `file-too-long` (a line count) and
    /// `lazy-ignore` (a marker scan) run over any language at all — the same
    /// text scan a `.txt` file gets, which is not knowledge of the language.
    /// See the [`coverage`] module docs.
    fn provides_language_lint(&self, language: &Language, cfg: &EngineConfig) -> bool {
        coverage::provides_language_lint(language, &Settings::from_config(cfg))
    }

    fn lint(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        let settings = Settings::from_config(cfg);
        if !settings.enabled {
            return Ok(Vec::new());
        }

        let mut diagnostics = Vec::new();

        if settings.file_too_long.enabled {
            metrics::check_file_too_long(src, settings.file_too_long.threshold, &mut diagnostics);
        }
        if settings.lazy_ignore {
            diagnostics.extend(lazy_ignore::scan(&src.content));
        }

        if let Some(grammar) = grammar_name(src) {
            run_structural_rules(&grammar, src, &settings, &mut diagnostics);
        }

        Ok(diagnostics)
    }
}

/// Resolve the language-pack grammar name for `src`, mirroring
/// `engines/treesitter/mod.rs::grammar_name`: prefer the pack's own
/// path-based detection (so `.jsx` resolves to the `javascript` grammar,
/// which has no separate grammar of its own — confirmed empirically), then
/// fall back to [`coverage::grammar_for`], which maps a [`Language`] (the
/// tier-1 variants and the [`Language::Other`] tail alike) to the same
/// grammar name [`Engine::provides_language_lint`] answers for — so the
/// grammar a file is parsed with and the grammar coverage was claimed for
/// can never disagree.
fn grammar_name(src: &SourceFile) -> Option<String> {
    let path = src.path.to_string_lossy();
    if let Some(name) = detect_language(&path) {
        return Some(name.to_owned());
    }
    Some(coverage::grammar_for(&src.language).to_owned())
}

/// Parse `src` once (via the pooled parser for `grammar`) and run every
/// structural rule against the resulting tree, degrading honestly per rule
/// per the module docs when the grammar has no query/table for it.
fn run_structural_rules(grammar: &str, src: &SourceFile, settings: &Settings, diagnostics: &mut Vec<Diagnostic>) {
    PARSERS.with(|cell| {
        let mut pool = cell.borrow_mut();
        if !pool.contains_key(grammar) {
            let Ok(language) = tree_sitter_language_pack::get_language(grammar) else {
                return;
            };
            let mut parser = Parser::new();
            if parser.set_language(&language).is_err() {
                return;
            }
            pool.insert(grammar.to_owned(), parser);
        }
        let Some(parser) = pool.get_mut(grammar) else {
            return;
        };
        let Some(tree) = parser.parse(src.content.as_bytes(), None) else {
            return;
        };
        let root = tree.root_node();
        let source = src.content.as_bytes();

        metrics::check_definitions(grammar, root, source, &src.language, settings, diagnostics);

        if settings.nesting_too_deep.enabled && !family::is_deferred(Rule::NestingTooDeep, &src.language) {
            for finding in nesting::analyze(grammar, root, usize_threshold(settings.nesting_too_deep.threshold)) {
                diagnostics.push(metrics::nesting_diagnostic(&finding, &src.content));
            }
        }
        if settings.cyclomatic_complexity.enabled && !family::is_deferred(Rule::CyclomaticComplexity, &src.language) {
            for finding in complexity::analyze(grammar, root, usize_threshold(settings.cyclomatic_complexity.threshold))
            {
                diagnostics.push(metrics::complexity_diagnostic(&finding, &src.content));
            }
        }
        if settings.magic_number {
            diagnostics.extend(magic_number::scan(root, source, &settings.magic_number_allow));
        }
        if settings.law_of_demeter {
            diagnostics.extend(demeter::scan(root, usize_threshold(settings.law_of_demeter_depth)));
        }
    });
}

/// Clamp a configured threshold to a non-negative `usize`. A negative or
/// zero threshold from a hostile/typo'd config degrades to "flag
/// everything" rather than panicking on the cast.
fn usize_threshold(value: i64) -> usize {
    usize::try_from(value).unwrap_or(0)
}

#[cfg(test)]
mod tests;
