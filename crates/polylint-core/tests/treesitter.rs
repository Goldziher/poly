//! Fixtures for the tree-sitter generic tier.
//!
//! - `generic_reindents_go` — proves the generic formatter structurally
//!   reindents a brace-delimited language (Go) with no language toolchain
//!   installed. The grammar is fetched on demand by the language pack, so this
//!   test requires network access on a cold cache (the project's accepted
//!   grammar-sourcing model).
//! - `generic_normalizes_whitespace` — a hermetic fixture (no grammar needed)
//!   proving whitespace normalization for a non-brace language.
//! - `r_format_normalizes_trailing_whitespace` — R language via the generic
//!   tier: known-unformatted fixture asserting exact formatted output.
//! - `r_lint_flags_trailing_whitespace` — R language via the generic tier:
//!   known-bad fixture asserting the expected trailing-whitespace Diagnostic.

use polylint_core::{
    Language,
    config::{EngineConfig, GlobalDefaults},
    engine::{Engine, FormatOutput, Severity, SourceFile},
    engines::treesitter::TreeSitterEngine,
};

fn engine_cfg(indent_width: usize) -> EngineConfig {
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width,
        options: toml::Table::new(),
    }
}

fn make_src(path: &str, language: Language, content: &str) -> SourceFile {
    SourceFile {
        path: path.into(),
        language,
        content: content.into(),
    }
}

const GO_UNFORMATTED: &str = "\
package main
import \"fmt\"
func main() {
if true {
fmt.Println(\"hi\")
}
}
";

#[test]
fn generic_reindents_go() {
    let engine = TreeSitterEngine;
    let src = make_src("main.go", Language::Other("go".into()), GO_UNFORMATTED);
    let formatted = match engine.format(&src, &engine_cfg(4)).unwrap() {
        FormatOutput::Formatted(text) => text,
        FormatOutput::Unchanged => GO_UNFORMATTED.to_string(),
    };
    insta::assert_snapshot!("generic_reindents_go", formatted);
}

// ---------------------------------------------------------------------------
// R language via tier-2 generic engine
// ---------------------------------------------------------------------------

/// Known-unformatted R: trailing spaces on line 1.  The generic tier trims
/// them via whitespace normalization, giving the clean output below.
const R_KNOWN_UNFORMATTED: &str = "f <- function(x) {   \n  x + 1\n}\n";

#[test]
fn r_format_normalizes_trailing_whitespace() {
    let engine = TreeSitterEngine;
    let src = make_src("example.R", Language::R, R_KNOWN_UNFORMATTED);
    match engine.format(&src, &engine_cfg(2)).unwrap() {
        FormatOutput::Formatted(text) => {
            insta::assert_snapshot!("r_format_normalizes_trailing_whitespace", text);
        }
        FormatOutput::Unchanged => {
            panic!("expected Formatted for R with trailing whitespace, got Unchanged")
        }
    }
}

/// Known-bad R: trailing whitespace triggers a trailing-whitespace Diagnostic.
#[test]
fn r_lint_flags_trailing_whitespace() {
    let engine = TreeSitterEngine;
    let src = make_src("example.R", Language::R, R_KNOWN_UNFORMATTED);
    let diags = engine.lint(&src, &engine_cfg(2)).unwrap();

    assert!(
        !diags.is_empty(),
        "expected trailing-whitespace diagnostic for R with trailing spaces"
    );
    let first = &diags[0];
    assert_eq!(first.engine, "treesitter");
    assert_eq!(first.code.as_deref(), Some("trailing-whitespace"));
    assert_eq!(first.severity, Severity::Warning);
    assert!(first.span.is_some(), "diagnostic must carry a source span");

    // Snapshot the diagnostic summary (not column numbers, which vary).
    let summary: Vec<_> = diags
        .iter()
        .map(|d| (d.engine.as_str(), d.code.as_deref().unwrap_or(""), d.severity))
        .collect();
    insta::assert_debug_snapshot!("r_lint_flags_trailing_whitespace", summary);
}

const WS_UNNORMALIZED: &str = "first line   \n\n\n\nsecond line\t\n";

#[test]
fn generic_normalizes_whitespace() {
    // An unknown grammar id never enters the brace-reindent path, so this needs
    // no grammar download — purely whitespace normalization.
    let engine = TreeSitterEngine;
    let src = make_src(
        "notes.unknownext",
        Language::Other("no-such-grammar".into()),
        WS_UNNORMALIZED,
    );
    let formatted = match engine.format(&src, &engine_cfg(2)).unwrap() {
        FormatOutput::Formatted(text) => text,
        FormatOutput::Unchanged => WS_UNNORMALIZED.to_string(),
    };
    insta::assert_snapshot!("generic_normalizes_whitespace", formatted);
}
