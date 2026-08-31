//! End-to-end coverage for the `invalid-config-value` check (issue #16).
//!
//! A key spelled correctly but given a value of the wrong *type* is read with a
//! typed accessor (`as_integer`, `as_bool`, …), which answers `None` and lets
//! the backend fall back to its default. Nothing said so: the config looked
//! enforced and was not. These tests pin the reported form of that finding
//! through the real pipeline, so the check cannot be satisfied by a unit test
//! against a backend that is never wired in.

use std::fs;
use std::path::Path;

use poly_core::{Config, Diagnostic, RunOptions};
use tempfile::tempdir;

fn options() -> RunOptions {
    RunOptions {
        no_cache: true,
        jobs: Some(1),
        exclude: Vec::new(),
        force_exclude: false,
        fix_generated: false,
        generated: None,
        explicit_config: false,
        config_resolver: None,
        externally_linted_languages: Vec::new(),
        only: Vec::new(),
        skip: Vec::new(),
    }
}

/// Lint a repo whose only file is the given `poly.toml`, returning the
/// `invalid-config-value` findings reported against it.
fn invalid_value_findings(config_source: &str) -> Vec<Diagnostic> {
    findings_with_code(config_source, "invalid-config-value")
}

fn findings_with_code(config_source: &str, code: &str) -> Vec<Diagnostic> {
    let repo = tempdir().unwrap();
    let root = repo.path();
    let config_path = root.join("poly.toml");
    fs::write(&config_path, config_source).unwrap();

    let config = Config::load(root).expect("load config");
    let results = poly_core::lint(std::slice::from_ref(&config_path), &config, &options(), false, false).unwrap();

    results
        .iter()
        .filter(|result| result.path == config_path)
        .flat_map(|result| result.diagnostics.iter())
        .filter(|diagnostic| diagnostic.code.as_deref() == Some(code))
        .cloned()
        .collect()
}

fn describe(findings: &[Diagnostic]) -> Vec<String> {
    findings.iter().map(|d| d.title.clone()).collect()
}

/// The reproducer from issue #16, verbatim.
#[test]
fn a_string_written_where_an_integer_is_read_is_reported_naming_the_key() {
    let found = invalid_value_findings("[lint.python.ruff]\nselect = [\"ALL\"]\nmccabe_max_complexity = \"oops\"\n");

    assert_eq!(
        found.len(),
        1,
        "expected exactly one finding, got {:?}",
        describe(&found)
    );
    let finding = &found[0];
    assert!(
        finding.title.contains("mccabe_max_complexity"),
        "the finding must name the key: {}",
        finding.title
    );
    assert!(
        finding.title.contains("integer") && finding.title.contains("string"),
        "the finding must name the expected and the found type: {}",
        finding.title
    );
    assert_eq!(finding.severity, poly_core::Severity::Warning);
    let description = finding.description.as_deref().expect("a description");
    assert!(
        description.contains("discard") || description.contains("no effect"),
        "the finding must state the consequence: {description}"
    );
}

/// The counterweight: the same key with a usable value reports nothing, so the
/// test above cannot pass by warning about everything.
#[test]
fn the_same_key_with_a_usable_value_is_not_reported() {
    let found = invalid_value_findings("[lint.python.ruff]\nselect = [\"ALL\"]\nmccabe_max_complexity = 1\n");
    assert!(found.is_empty(), "unexpected findings: {:?}", describe(&found));
}

/// A wrong-typed value is a *different* finding from a misspelled key: they are
/// separately nameable in `[per-file-ignores]` and rule selection.
#[test]
fn a_wrong_typed_value_does_not_report_as_an_unknown_key() {
    let source = "[lint.python.ruff]\nmccabe_max_complexity = \"oops\"\n";
    assert!(
        findings_with_code(source, "unknown-config-key").is_empty(),
        "a known key must not be reported as unknown just because its value is unusable"
    );
    assert_eq!(findings_with_code(source, "invalid-config-value").len(), 1);
}

/// The finding points at the offending key, like the unknown-key one does.
#[test]
fn the_finding_points_at_the_offending_key() {
    let found = invalid_value_findings("[defaults]\nline_length = 120\n\n[fmt.toml.taplo]\nreorder_keys = 4\n");
    assert_eq!(found.len(), 1, "{:?}", describe(&found));
    let span = found[0].span.expect("a span");
    assert_eq!((span.start_line, span.start_col), (5, 1));
}

/// The check is not TOML-wide: a `Cargo.toml` is not a poly config.
#[test]
fn a_toml_file_that_is_not_a_poly_config_is_left_alone() {
    let repo = tempdir().unwrap();
    let root = repo.path();
    fs::write(root.join("poly.toml"), "").unwrap();
    let cargo = root.join("Cargo.toml");
    fs::write(&cargo, "[lint.python.ruff]\nmccabe_max_complexity = \"oops\"\n").unwrap();

    let config = Config::load(root).expect("load config");
    let results = poly_core::lint(std::slice::from_ref(&cargo), &config, &options(), false, false).unwrap();
    let found: Vec<&Diagnostic> = results
        .iter()
        .flat_map(|result| result.diagnostics.iter())
        .filter(|d| d.code.as_deref() == Some("invalid-config-value"))
        .collect();
    assert!(
        found.is_empty(),
        "unexpected findings in {}",
        Path::new("Cargo.toml").display()
    );
}
