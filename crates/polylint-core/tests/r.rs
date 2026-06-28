//! Insta snapshot fixtures for the air R backend.
//!
//! - `r_known_unformatted_snapshot` — a known-unformatted R snippet asserts the
//!   exact formatted output produced by `air_r_formatter` with polylint's defaults
//!   (line width 120, indent width 2, space indent).

use polylint_core::{
    Language,
    config::{EngineConfig, GlobalDefaults},
    engine::{Engine, FormatOutput, SourceFile},
    engines::r::REngine,
};

fn engine_cfg() -> EngineConfig {
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 2,
        options: toml::Table::new(),
    }
}

fn make_src(content: &str) -> SourceFile {
    SourceFile {
        path: "known_unformatted.R".into(),
        language: Language::R,
        content: content.into(),
    }
}

// ---------------------------------------------------------------------------
// Known-unformatted fixture: no spaces around `<-`, no spaces around `,`,
// inline function body — air should expand and space these canonically.
// ---------------------------------------------------------------------------

const KNOWN_UNFORMATTED: &str = "x<-1+2\nf<-function(a,b){a+b}\n";

#[test]
fn r_known_unformatted_snapshot() {
    let engine = REngine;
    let src = make_src(KNOWN_UNFORMATTED);
    let result = engine.format(&src, &engine_cfg()).unwrap();

    let formatted = match result {
        FormatOutput::Formatted(s) => s,
        FormatOutput::Unchanged => {
            panic!("expected Formatted for known-unformatted input; got Unchanged")
        }
    };

    insta::assert_snapshot!("r_known_unformatted_output", formatted);
}

// ---------------------------------------------------------------------------
// Already-formatted round-trip: feed the canonical output back in and confirm
// it returns Unchanged (idempotent formatter).
// ---------------------------------------------------------------------------

#[test]
fn r_already_formatted_is_unchanged() {
    let engine = REngine;
    // Canonical air output for the known-unformatted snippet above.
    let canonical = "x <- 1 + 2\nf <- function(a, b) {\n  a + b\n}\n";
    let src = make_src(canonical);
    let result = engine.format(&src, &engine_cfg()).unwrap();
    assert!(
        matches!(result, FormatOutput::Unchanged),
        "expected Unchanged for already-formatted input"
    );
}
