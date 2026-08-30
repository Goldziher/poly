//! Insta snapshot fixture for the `quality` cross-cutting metric engine
//! (ADR 0027, phase 3a).
//!
//! - `known_bad_quality_diagnostics` — a Rust fixture (Rust has no tier-1
//!   lint backend, so every rule in the OTHER family runs at its default or
//!   near-default threshold) asserting the expected [`Diagnostic`]s:
//!   `too-many-parameters`, `nesting-too-deep`, `cyclomatic-complexity` and
//!   `function-too-long`.
//! - `known_bad_lazy_ignore_diagnostics` — a Python fixture for the one
//!   remaining rule the Rust fixture cannot reach. Since the 2026-08-30
//!   amendment to ADR 0027, `lazy-ignore` no longer scans Rust
//!   `#[allow(..)]` (that belongs to the `allow-attribute-without-reason`
//!   pack rule), and every marker it still scans for is another tool's
//!   suppression syntax.
//! - `clean_file_has_no_quality_diagnostics` — verifies clean input produces
//!   no findings.
//!
//! `quality` declares `format: false` (it is lint-only, see
//! `engines/quality/mod.rs`), so there is no matching known-unformatted
//! fixture — `Engine::format` always returns `Unchanged` for it, which is
//! covered by `format_is_always_a_no_op` below instead.

use poly_core::{
    Language,
    config::{EngineConfig, GlobalDefaults},
    engine::{Engine, FormatOutput, SourceFile},
    engines::quality::QualityEngine,
};

fn engine_cfg(options: toml::Table) -> EngineConfig {
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 4,
        options,
    }
}

fn make_src(content: &str) -> SourceFile {
    SourceFile {
        path: "fixture.rs".into(),
        language: Language::Rust,
        content: content.into(),
    }
}

const KNOWN_BAD: &str = include_str!("fixtures/quality/known_bad.rs");
const KNOWN_BAD_LAZY_IGNORE: &str = include_str!("fixtures/quality/known_bad_lazy_ignore.py");

#[test]
fn known_bad_quality_diagnostics() {
    let engine = QualityEngine;
    // Lower two thresholds so this fixture stays small and readable rather
    // than needing 20 real branches or 80 real lines to prove each rule
    // fires; `too-many-parameters` (default 6) and `nesting-too-deep`
    // (default 4) already fire on the fixture as written.
    let mut options = toml::Table::new();
    options.insert("cyclomatic_complexity_max".to_owned(), toml::Value::Integer(3));
    options.insert("function_too_long_lines".to_owned(), toml::Value::Integer(5));
    let cfg = engine_cfg(options);

    let src = make_src(KNOWN_BAD);
    let diags = engine.lint(&src, &cfg).unwrap();

    assert!(!diags.is_empty(), "expected quality diagnostics for known-bad file");
    for diag in &diags {
        assert_eq!(diag.engine, "quality");
        assert_eq!(diag.severity, poly_core::engine::Severity::Warning);
    }

    let mut summary: Vec<_> = diags
        .iter()
        .map(|d| {
            (
                d.code.as_deref().unwrap_or(""),
                d.span.as_ref().map(|s| (s.start_line, s.start_col)),
            )
        })
        .collect();
    summary.sort();
    insta::assert_debug_snapshot!("known_bad_quality_diagnostics", summary);
}

/// The `lazy-ignore` half of the known-bad bar. Asserted as an exact
/// (code, line, column) list, not merely "something fired", so that a marker
/// silently dropped from the scan — the change this fixture was split out
/// for — fails here rather than passing as a smaller set.
#[test]
fn known_bad_lazy_ignore_diagnostics() {
    let engine = QualityEngine;
    let src = SourceFile {
        path: "fixture.py".into(),
        language: Language::Python,
        content: KNOWN_BAD_LAZY_IGNORE.into(),
    };
    let diags = engine.lint(&src, &engine_cfg(toml::Table::new())).unwrap();

    let summary: Vec<_> = diags
        .iter()
        .map(|d| {
            (
                d.code.as_deref().unwrap_or(""),
                d.span.as_ref().map(|s| (s.start_line, s.start_col)),
            )
        })
        .collect();
    insta::assert_debug_snapshot!("known_bad_lazy_ignore_diagnostics", summary);
}

/// Every threshold rule must name its ceiling in the message. A reader who
/// disagrees with a finding has exactly one next step — change the number in
/// `poly.toml` — and cannot take it if the message does not say what the
/// number currently is, or how far over it they are. `nesting-too-deep` said
/// "(max exceeded)" and `cyclomatic-complexity` said nothing at all until this
/// was pinned.
#[test]
fn every_threshold_rule_reports_the_configured_maximum() {
    let engine = QualityEngine;
    let mut options = toml::Table::new();
    options.insert("cyclomatic_complexity_max".to_owned(), toml::Value::Integer(3));
    options.insert("function_too_long_lines".to_owned(), toml::Value::Integer(5));
    let diags = engine.lint(&make_src(KNOWN_BAD), &engine_cfg(options)).unwrap();

    let threshold_rules = [
        "file-too-long",
        "function-too-long",
        "type-too-long",
        "too-many-parameters",
        "nesting-too-deep",
        "cyclomatic-complexity",
    ];
    let mut seen: Vec<&str> = Vec::new();
    for diag in &diags {
        let code = diag.code.as_deref().unwrap_or("");
        if !threshold_rules.contains(&code) {
            continue;
        }
        seen.push(code);
        assert!(
            diag.title.contains("(max "),
            "{code} must name its ceiling, got {:?}",
            diag.title
        );
    }
    // Guard against the assertion passing vacuously if the fixture stops
    // triggering these rules.
    for rule in ["nesting-too-deep", "cyclomatic-complexity"] {
        assert!(seen.contains(&rule), "fixture no longer triggers {rule}: {diags:?}");
    }
}

#[test]
fn clean_file_has_no_quality_diagnostics() {
    let engine = QualityEngine;
    let src = make_src("fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n");
    let diags = engine.lint(&src, &engine_cfg(toml::Table::new())).unwrap();
    assert!(
        diags.is_empty(),
        "expected no diagnostics for clean file, got: {diags:?}"
    );
}

/// `quality` is lint-only (`capabilities().format == false`); `format`
/// always returns `Unchanged` rather than echoing input, matching the
/// `Engine` trait contract.
#[test]
fn format_is_always_a_no_op() {
    let engine = QualityEngine;
    assert!(!engine.capabilities().format);
    let src = make_src(KNOWN_BAD);
    match engine.format(&src, &engine_cfg(toml::Table::new())).unwrap() {
        FormatOutput::Unchanged => {}
        FormatOutput::Formatted(_) => panic!("quality must never reformat source"),
    }
}

/// Coverage, at the engine's public surface and in both directions. `quality`
/// is registered for every language, so the interesting question is which of
/// them it will let a run count as linted: Rust and Kotlin have a full
/// structural model (definition query + construct table), Swift and Zig reach
/// only the language-agnostic floor. See `engines/quality/coverage.rs`.
#[test]
fn a_language_with_a_structural_model_claims_lint_coverage() {
    let cfg = engine_cfg(toml::Table::new());
    for language in [Language::Rust, Language::Kotlin, Language::Go, Language::CSharp] {
        assert!(
            QualityEngine.provides_language_lint(&language, &cfg),
            "{language:?} is structurally modelled, so quality lints it"
        );
    }
}

#[test]
fn a_language_that_only_gets_line_counting_claims_no_lint_coverage() {
    let cfg = engine_cfg(toml::Table::new());
    for language in [Language::Swift, Language::Zig, Language::Shell, Language::Markdown] {
        assert!(
            !QualityEngine.provides_language_lint(&language, &cfg),
            "{language:?} gets only file-too-long + lazy-ignore, which is not language knowledge"
        );
    }
}

/// The two halves are independent: an unmodelled language still *gets* the
/// floor rules and still reports their findings — it simply does not let the
/// run claim the file was linted, exactly as a `typos` finding never did.
#[test]
fn an_unmodelled_language_still_reports_the_floor_it_does_not_get_coverage_for() {
    let cfg = engine_cfg(toml::Table::new());
    let src = SourceFile {
        path: "fixture.zig".into(),
        language: Language::Zig,
        content: "x\n".repeat(1200).into(),
    };
    let diags = QualityEngine.lint(&src, &cfg).unwrap();
    assert!(
        diags.iter().any(|d| d.code.as_deref() == Some("file-too-long")),
        "the floor still runs for Zig: {diags:?}"
    );
    assert!(!QualityEngine.provides_language_lint(&Language::Zig, &cfg));
}
