//! Insta snapshot fixtures for the rubyfmt Ruby backend.
//!
//! - `should_reformat_misindented_ruby` — a Ruby method with loose argument
//!   spacing and bare `puts` calls asserts the exact formatted output produced
//!   by rubyfmt (parens normalized, spacing tightened).
//! - `should_return_unchanged_on_unparsable_ruby` — an unclosed `def`
//!   asserts `FormatOutput::Unchanged` (robustness rule: parse errors must
//!   not crash the run or corrupt the file).
//! - `the_committed_index_describes_the_ruby_component` — the committed component index entry
//!   for `ruby` equals what this crate's engines describe, since `poly-core` cannot link them.

use poly_component_index::ComponentIndex;
use poly_engine_api::{
    config::{EngineConfig, GlobalDefaults},
    desc::engine_desc,
    engine::{Engine, FormatOutput, SourceFile},
    language::Language,
};
use poly_engines_ruby::{RubyfmtEngine, engines_for};

fn engine_cfg() -> EngineConfig {
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 2,
        options: toml::Table::new(),
    }
}

fn make_src(path: &str, content: &str) -> SourceFile {
    SourceFile {
        path: path.into(),
        language: Language::Ruby,
        content: content.into(),
    }
}

/// Unformatted Ruby: spaces inside method-param parens and bare `puts`.
///
/// No trailing whitespace on any line so the `trailing-whitespace` prek
/// hook does not rewrite this literal.
const KNOWN_UNFORMATTED: &str = "def greet( name )\n  x = 1 + 2\n  if x > 3\n\n    puts \"hello\"\n  end\nend\n";

#[test]
fn should_reformat_misindented_ruby() {
    let engine = RubyfmtEngine;
    let src = make_src("greet.rb", KNOWN_UNFORMATTED);
    match engine.format(&src, &engine_cfg()).unwrap() {
        FormatOutput::Formatted(text) => {
            insta::assert_snapshot!("rubyfmt_known_unformatted_output", text);
        }
        FormatOutput::Unchanged => panic!("expected Formatted, got Unchanged"),
    }
}

#[test]
fn should_return_unchanged_on_unparsable_ruby() {
    let engine = RubyfmtEngine;
    let src = make_src("bad.rb", "def foo(");
    let out = engine.format(&src, &engine_cfg()).unwrap();
    assert!(
        matches!(out, FormatOutput::Unchanged),
        "expected Unchanged for syntax-error input, got Formatted"
    );
}

#[test]
fn the_provider_serves_ruby_only() {
    let ruby = engines_for(&Language::Ruby);
    assert_eq!(ruby.iter().map(|e| e.name()).collect::<Vec<_>>(), ["rubyfmt"]);
    assert!(engines_for(&Language::Python).is_empty());
}

#[test]
fn the_committed_index_describes_the_ruby_component() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../crates/poly-component-index/data/index.json");
    let text = std::fs::read_to_string(&path).expect("index.json is committed");
    let mut index = ComponentIndex::parse(&text).expect("index.json parses");
    let described: Vec<_> = engines_for(&Language::Ruby)
        .iter()
        .map(|engine| engine_desc(engine.as_ref()))
        .collect();
    if std::env::var_os("UPDATE_INDEX").is_some() {
        index
            .components
            .iter_mut()
            .find(|c| c.name == "ruby")
            .expect("the index declares the ruby component")
            .engines = described;
        let mut rendered = serde_json::to_string_pretty(&index).expect("index serializes");
        rendered.push('\n');
        std::fs::write(&path, rendered).expect("write index.json");
        return;
    }
    let component = index.component("ruby").expect("the index declares the ruby component");
    assert_eq!(
        component.engines, described,
        "the committed index is stale for ruby; regenerate it with \
         `UPDATE_INDEX=1 cargo test --manifest-path components/Cargo.toml -p poly-engines-ruby --test rubyfmt`, \
         then `poly fmt --fix` the file"
    );
}
