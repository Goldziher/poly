//! Fixtures for the tree-sitter generic tier.
//!
//! - `generic_reindents_go` — proves the generic formatter structurally
//!   reindents a brace-delimited language (Go) with no language toolchain
//!   installed. The grammar is fetched on demand by the language pack, so this
//!   test requires network access on a cold cache (the project's accepted
//!   grammar-sourcing model).
//! - `generic_normalizes_whitespace` — a hermetic fixture (no grammar needed)
//!   proving whitespace normalization for a non-brace language.

use polylint_core::{
    Language,
    config::{EngineConfig, GlobalDefaults},
    engine::{Engine, FormatOutput, SourceFile},
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
        content: content.to_string(),
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
