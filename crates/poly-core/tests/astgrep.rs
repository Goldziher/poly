//! Fixtures for the ast-grep custom-rule tier (`AstGrepEngine`).
//!
//! - `python_rule_flags_and_fixes` — a user rule matches `print(...)` in Python,
//!   producing a `Diagnostic` with a byte-range autofix that rewrites it to
//!   `log(...)`. Proves the tier-1-language path plus the `NodeMatch` → `Edit`
//!   byte mapping.
//! - `go_rule_runs_on_tier2_grammar` — a rule matches `fmt.Println(...)` in Go,
//!   a language with NO native poly backend. Proves the TSLP↔ast-grep grammar
//!   bridge works for the generic tier with zero system tools installed.
//! - `no_rules_dir_is_a_noop` / `valid_source_has_no_diagnostics` — the engine
//!   is a quiet no-op when there is nothing to say.
//!
//! Rule YAML is written to a temp dir at runtime (not committed fixture files)
//! so the repo's own fixable pre-commit hooks cannot rewrite the rule bodies.

use std::fs;
use std::path::Path;

use poly_core::{
    Language,
    config::{EngineConfig, GlobalDefaults},
    engine::{Diagnostic, Engine, SourceFile},
    engines::astgrep::AstGrepEngine,
};

/// Build an `EngineConfig` whose `options` point the engine at `dir` for rules.
///
/// Injects `rules_hash` alongside `rules_dirs`, exactly as
/// `Config::build_astgrep_options` does in production, so the content-addressed
/// rule cache path is exercised (rather than the empty-hash bypass).
fn cfg_with_rules_dir(dir: &Path) -> EngineConfig {
    let dirs = vec![dir.to_string_lossy().into_owned()];
    let mut options = toml::Table::new();
    options.insert(
        "rules_dirs".to_string(),
        toml::Value::Array(dirs.iter().cloned().map(toml::Value::String).collect()),
    );
    let hash = poly_core::engines::astgrep::rules::rules_hash(&dirs);
    if !hash.is_empty() {
        options.insert("rules_hash".to_string(), toml::Value::String(hash));
    }
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 4,
        options,
    }
}

fn make_src(path: &str, language: Language, content: &str) -> SourceFile {
    SourceFile {
        path: path.into(),
        language,
        content: content.into(),
    }
}

/// Apply a diagnostic's fix edits to `source`, rightmost-first (so earlier byte
/// offsets stay valid), returning the rewritten string.
fn apply_fix(source: &str, diag: &Diagnostic) -> String {
    let mut edits = diag.fix.clone();
    edits.sort_by_key(|e| std::cmp::Reverse(e.start_byte));
    let mut out = source.to_string();
    for e in edits {
        out.replace_range(e.start_byte..e.end_byte, &e.replacement);
    }
    out
}

#[test]
fn python_rule_flags_and_fixes() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("no-print.yml"),
        "id: no-print\nlanguage: python\nrule:\n  pattern: print($MSG)\nmessage: use logging, not print\nseverity: warning\nfix: log($MSG)\n",
    )
    .unwrap();

    let engine = AstGrepEngine;
    let cfg = cfg_with_rules_dir(dir.path());
    let source = "print(\"hello\")\n";
    let src = make_src("m.py", Language::Python, source);

    let diags = engine.lint(&src, &cfg).unwrap();

    let hit = diags
        .iter()
        .find(|d| d.code.as_deref() == Some("no-print"))
        .unwrap_or_else(|| panic!("expected no-print diagnostic; got: {diags:?}"));

    // Structural assertions on the diagnostic.
    assert_eq!(hit.engine, "astgrep");
    assert_eq!(hit.severity, poly_core::engine::Severity::Warning);
    assert!(hit.span.is_some(), "diagnostic must carry a span");
    assert_eq!(hit.span.as_ref().unwrap().start_line, 1);

    // The autofix must actually rewrite print(...) → log(...).
    assert!(!hit.fix.is_empty(), "no-print rule declares a fix; edits expected");
    assert_eq!(apply_fix(source, hit), "log(\"hello\")\n");
}

#[test]
fn go_rule_runs_on_tier2_grammar() {
    // Go has no native poly backend — this exercises the TSLP↔ast-grep bridge
    // on the tree-sitter generic tier.
    // A bare Go expression is not valid at file top level, so ast-grep fragments
    // need the `context`/`selector` form for Go — standard ast-grep usage. This
    // exercises the TSLP↔ast-grep bridge on a grammar with no native backend.
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("no-println.yml"),
        "id: no-println\nlanguage: go\nrule:\n  pattern:\n    selector: call_expression\n    context: \"func f() { fmt.Println($A) }\"\nmessage: use the structured logger\nseverity: error\n",
    )
    .unwrap();

    let engine = AstGrepEngine;
    let cfg = cfg_with_rules_dir(dir.path());
    let source = "package main\n\nimport \"fmt\"\n\nfunc main() {\n\tfmt.Println(\"hi\")\n}\n";
    let src = make_src("main.go", Language::Other("go".to_string()), source);

    let diags = engine.lint(&src, &cfg).unwrap();
    assert!(
        diags.iter().any(|d| d.code.as_deref() == Some("no-println")),
        "Go rule must fire via the TSLP bridge; got: {diags:?}"
    );
}

#[test]
fn valid_source_has_no_diagnostics() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("no-print.yml"),
        "id: no-print\nlanguage: python\nrule:\n  pattern: print($$$ARGS)\nmessage: use logging\nseverity: warning\n",
    )
    .unwrap();

    let engine = AstGrepEngine;
    let cfg = cfg_with_rules_dir(dir.path());
    let src = make_src("ok.py", Language::Python, "x = 1\n");

    let diags = engine.lint(&src, &cfg).unwrap();
    assert!(diags.is_empty(), "clean source must produce nothing; got: {diags:?}");
}

/// The rule library shipped in the repo's top-level `rules/` directory must
/// pass its own `*-test.yml` cases — this is what `poly rules test rules/` runs,
/// wired into CI so a broken rule or test is caught here.
#[test]
fn shipped_rule_library_passes_its_tests() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../rules").to_string();
    let report = poly_core::engines::astgrep::test::run_tests(&[root]).unwrap();

    assert!(
        report.missing_rule_ids.is_empty(),
        "test files name unknown rule ids: {:?}",
        report.missing_rule_ids
    );
    assert!(
        report.passed() > 0,
        "expected the rule library to run some snippet checks"
    );
    assert_eq!(
        report.failed(),
        0,
        "shipped rule library has failing snippets: {:?}",
        report
            .outcomes
            .iter()
            .filter(|o| !o.passed)
            .map(|o| (&o.rule_id, o.kind, o.index))
            .collect::<Vec<_>>()
    );
}

/// A rule with `severity: off` is disabled and must emit nothing, even though
/// its pattern matches (regression for the code-review finding that `Off` was
/// mapped to a live `Hint`).
#[test]
fn off_severity_rule_emits_nothing() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("off.yml"),
        "id: off-rule\nlanguage: python\nseverity: off\nmessage: disabled\nrule:\n  pattern: print($MSG)\n",
    )
    .unwrap();

    let engine = AstGrepEngine;
    let cfg = cfg_with_rules_dir(dir.path());
    let src = make_src("m.py", Language::Python, "print(\"x\")\n");

    let diags = engine.lint(&src, &cfg).unwrap();
    assert!(diags.is_empty(), "severity: off must suppress the rule; got: {diags:?}");
}

/// Span columns are character-based, not byte-based: a multi-byte char before
/// the match must not inflate the reported column (regression for `byte_point`).
#[test]
fn span_column_is_character_based() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("find.yml"),
        "id: find-print\nlanguage: python\nseverity: warning\nmessage: hit\nrule:\n  pattern: print($MSG)\n",
    )
    .unwrap();

    let engine = AstGrepEngine;
    let cfg = cfg_with_rules_dir(dir.path());
    // `é` (U+00E9) is 1 char but 2 bytes; `print` starts at char index 9 → col 10.
    // A byte-based column would report 11.
    let src = make_src("m.py", Language::Python, "x = \"é\"; print(1)\n");

    let diags = engine.lint(&src, &cfg).unwrap();
    let hit = diags
        .iter()
        .find(|d| d.code.as_deref() == Some("find-print"))
        .unwrap_or_else(|| panic!("expected find-print; got: {diags:?}"));
    assert_eq!(
        hit.span.as_ref().unwrap().start_col,
        10,
        "expected character column 10 (byte-based would be 11)"
    );
}

/// A `fixed:` assertion in an `invalid` test case passes when the rule's
/// applied autofix matches, and fails when it does not — proving the rule-test
/// runner checks fix output, not just that the rule fires.
#[test]
fn rule_test_fixed_assertion_checks_autofix_output() {
    use poly_core::engines::astgrep::test::{CaseKind, run_tests};

    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("use-is-none.yml"),
        "id: use-is-none\nlanguage: python\nseverity: warning\nmessage: use is None\nrule:\n  pattern: $X == None\nfix: $X is None\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("use-is-none-test.yml"),
        // First case asserts the correct fix; second asserts a wrong one to
        // prove a mismatch is caught.
        "id: use-is-none\ninvalid:\n  - code: a == None\n    fixed: a is None\n  - code: b == None\n    fixed: b == None\n",
    )
    .unwrap();

    let report = run_tests(&[dir.path().to_string_lossy().into_owned()]).unwrap();

    let fixed: Vec<_> = report.outcomes.iter().filter(|o| o.kind == CaseKind::Fixed).collect();
    assert_eq!(fixed.len(), 2, "one Fixed outcome per fixed: assertion; got {report:?}");
    assert!(fixed[0].passed, "correct fix must pass: {:?}", fixed[0]);
    assert!(!fixed[1].passed, "wrong fix must fail: {:?}", fixed[1]);
    assert!(
        fixed[1].detail.as_deref().is_some_and(|d| d.contains("b is None")),
        "mismatch detail should show the actual fix output: {:?}",
        fixed[1].detail,
    );
}

/// A `fixed:` case whose snippet does NOT match the rule reports exactly one
/// failure (the `Invalid` match check), not a second misleading `Fixed` failure
/// for the fix that never ran.
#[test]
fn non_matching_fixed_case_reports_single_failure() {
    use poly_core::engines::astgrep::test::{CaseKind, run_tests};

    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("use-is-none.yml"),
        "id: use-is-none\nlanguage: python\nseverity: warning\nmessage: use is None\nrule:\n  pattern: $X == None\nfix: $X is None\n",
    )
    .unwrap();
    // `x is None` does NOT match `$X == None`, so the Invalid check fails; the
    // Fixed check must be skipped.
    fs::write(
        dir.path().join("use-is-none-test.yml"),
        "id: use-is-none\ninvalid:\n  - code: x is None\n    fixed: x is None\n",
    )
    .unwrap();

    let report = run_tests(&[dir.path().to_string_lossy().into_owned()]).unwrap();
    let failures: Vec<_> = report.outcomes.iter().filter(|o| !o.passed).collect();
    assert_eq!(failures.len(), 1, "exactly one failure expected; got {report:?}");
    assert_eq!(failures[0].kind, CaseKind::Invalid);
}

#[test]
fn no_rules_dir_is_a_noop() {
    let engine = AstGrepEngine;
    // No `rules_dirs` option at all → engine short-circuits.
    let cfg = EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 4,
        options: toml::Table::new(),
    };
    let src = make_src("m.py", Language::Python, "print(1)\n");
    let diags = engine.lint(&src, &cfg).unwrap();
    assert!(
        diags.is_empty(),
        "no rules dir configured → no diagnostics; got: {diags:?}"
    );
}
