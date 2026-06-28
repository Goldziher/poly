//! Insta snapshot fixtures for the air/jarl R backend.
//!
//! - `r_known_bad_diagnostics` — a known-bad R snippet (`x == NA`) asserts the
//!   expected [`Diagnostic`]s structurally (engine, code, severity, fix present).
//! - `r_known_unformatted_snapshot` — a known-unformatted R snippet asserts the
//!   exact formatted output produced by `air_r_formatter` with polylint's defaults
//!   (line width 120, indent width 2, space indent).

use polylint_core::{
    Language,
    config::{EngineConfig, GlobalDefaults},
    engine::{Diagnostic, Engine, FormatOutput, SourceFile},
    engines::r::REngine,
};

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
        language: Language::R,
        content: content.into(),
    }
}

// ---------------------------------------------------------------------------
// Known-bad fixture: `x == NA` triggers the `equals_na` rule.
//
// The structural snapshot captures (engine, code, severity, fix_present) so
// it is stable across jarl message-text changes.  The snapshot is stored under
// tests/snapshots/ and excluded from the typos/trailing-whitespace prek hooks.
// ---------------------------------------------------------------------------

/// R code with a known-bad pattern: comparing to NA via `==` should use `is.na()`.
const KNOWN_BAD: &str = "x <- c(1, 2, NA)\ny <- x == NA\n";

#[test]
fn r_known_bad_diagnostics() {
    let engine = REngine;
    let src = make_src("known_bad.R", KNOWN_BAD);
    let diags: Vec<Diagnostic> = engine.lint(&src, &engine_cfg()).unwrap();

    // Filter to equals_na so the snapshot is stable even if other rules fire.
    let relevant: Vec<_> = diags
        .iter()
        .filter(|d| d.code.as_deref() == Some("equals_na"))
        .collect();

    assert!(
        !relevant.is_empty(),
        "expected at least one equals_na diagnostic from jarl, got none.\nAll diags: {diags:?}"
    );

    // Structural summary: (engine, code, severity, fix_present).
    // This form is stable across jarl version bumps that change message text.
    let summary: Vec<_> = relevant
        .iter()
        .map(|d| {
            (
                d.engine.as_str(),
                d.code.as_deref().unwrap_or(""),
                d.severity,
                !d.fix.is_empty(),
            )
        })
        .collect();

    insta::assert_debug_snapshot!("r_known_bad_diagnostics", summary);
}

// ---------------------------------------------------------------------------
// Known-unformatted fixture: no spaces around `<-`, no spaces around `,`,
// inline function body — air should expand and space these canonically.
// ---------------------------------------------------------------------------

const KNOWN_UNFORMATTED: &str = "x<-1+2\nf<-function(a,b){a+b}\n";

#[test]
fn r_known_unformatted_snapshot() {
    let engine = REngine;
    let src = make_src("known_unformatted.R", KNOWN_UNFORMATTED);
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
    let src = make_src("canonical.R", canonical);
    let result = engine.format(&src, &engine_cfg()).unwrap();
    assert!(
        matches!(result, FormatOutput::Unchanged),
        "expected Unchanged for already-formatted input"
    );
}
