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
/// rule cache path is exercised (rather than the empty-hash bypass). Explicitly
/// disables the built-in pack: these fixtures test *user* rule behavior and
/// must stay isolated from the pack's own (evolving) content — see
/// `crates/poly-core/src/engines/astgrep/mod.rs` for pack-specific coverage.
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
    options.insert("builtin_pack_enabled".to_string(), toml::Value::Boolean(false));
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

    assert_eq!(hit.engine, "astgrep");
    assert_eq!(hit.severity, poly_core::engine::Severity::Warning);
    assert!(hit.span.is_some(), "diagnostic must carry a span");
    assert_eq!(hit.span.as_ref().unwrap().start_line, 1);

    assert!(!hit.fix.is_empty(), "no-print rule declares a fix; edits expected");
    assert_eq!(apply_fix(source, hit), "log(\"hello\")\n");
}

#[test]
fn go_rule_runs_on_tier2_grammar() {
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
    let mut options = toml::Table::new();
    // The built-in pack is on by default (see `builtin_pack_fires_by_default`
    // below); disabling it here isolates this test's original claim — no
    // *user* rules configured, and no pack either, is a true no-op — from the
    // pack's own (evolving) content.
    options.insert("builtin_pack_enabled".to_string(), toml::Value::Boolean(false));
    let cfg = EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 4,
        options,
    };
    let src = make_src("m.py", Language::Python, "print(1)\n");
    let diags = engine.lint(&src, &cfg).unwrap();
    assert!(
        diags.is_empty(),
        "no rules dir and no built-in pack → no diagnostics; got: {diags:?}"
    );
}

/// With no config at all — no `[rules] dirs`, no explicit `builtin` toggle —
/// the built-in pack is on by default: a Rust file whose `Err(_)` arm
/// silently discards the error gets flagged with zero setup. Complements the
/// unit test of the same shape in `engines::astgrep::tests` by exercising the
/// public `Engine` trait object from outside the crate, the way the runner
/// actually calls it. (`unwrap-used`, `allow-attribute-without-reason`, and
/// `undocumented-unsafe-block`, the earlier exemplars here, all shipped `off`
/// after this pack's default-on audit — see their YAML notes.)
#[test]
fn builtin_pack_fires_by_default() {
    let engine = AstGrepEngine;
    let cfg = EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 4,
        options: toml::Table::new(),
    };
    let src = make_src(
        "m.rs",
        Language::Rust,
        "fn f(r: Result<(), ()>) {\n    match r {\n        Ok(_) => {}\n        Err(_) => {}\n    }\n}\n",
    );
    let diags = engine.lint(&src, &cfg).unwrap();
    assert!(
        diags.iter().any(|d| d.code.as_deref() == Some("swallowed-error")),
        "expected the default-on built-in swallowed-error rule to fire with no config; got: {diags:?}"
    );
}

/// A `.jsx` file must be reachable by ast-grep rules.
///
/// `Language::Jsx.id()` is `"jsx"`, which `tree-sitter-language-pack` does not
/// ship as a grammar (it declares `.jsx` an extension of `javascript`). Before
/// the grammar mapping, this meant `.jsx` had **no** ast-grep coverage from any
/// source: a rule declaring `language: jsx` failed to deserialize, and a rule
/// declaring `language: javascript` was keyed `"javascript"` and never looked
/// up for a `jsx` file. No configuration could fix it.
#[test]
fn javascript_rule_fires_on_a_jsx_file() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("no-console.yml"),
        "id: no-console-log\nlanguage: javascript\nseverity: warning\nmessage: drop the console call\nrule:\n  pattern: console.log($$$ARGS)\n",
    )
    .unwrap();

    let engine = AstGrepEngine;
    let cfg = cfg_with_rules_dir(dir.path());
    let source = "const App = () => <div>{value}</div>;\nconsole.log(App);\n";
    let src = make_src("App.jsx", Language::Jsx, source);

    let diags = engine.lint(&src, &cfg).unwrap();
    assert!(
        diags.iter().any(|d| d.code.as_deref() == Some("no-console-log")),
        "a `language: javascript` rule must fire on a .jsx file; got: {diags:?}"
    );
}

/// Coverage accounting must agree with what `lint` actually runs: with a
/// JavaScript rule loaded, JSX is linted, so the run may not report
/// `no lint rules for Jsx`.
#[test]
fn jsx_claims_lint_coverage_from_a_javascript_rule() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("no-console.yml"),
        "id: no-console-log\nlanguage: javascript\nseverity: warning\nmessage: drop the console call\nrule:\n  pattern: console.log($$$ARGS)\n",
    )
    .unwrap();

    let engine = AstGrepEngine;
    let cfg = cfg_with_rules_dir(dir.path());
    assert!(
        engine.provides_language_lint(&Language::Jsx, &cfg),
        "a JavaScript rule gives JSX ast-grep lint coverage"
    );
}

/// The other poly language ids that are not themselves grammar names resolve
/// through the same mapping: a rule written for the grammar that parses the
/// language fires on files of that language.
#[test]
fn json_rule_fires_on_a_jsonc_file() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("no-todo-key.yml"),
        "id: no-todo-key\nlanguage: json\nseverity: warning\nmessage: placeholder key\nrule:\n  pattern: \"\\\"todo\\\"\"\n",
    )
    .unwrap();

    let engine = AstGrepEngine;
    let cfg = cfg_with_rules_dir(dir.path());
    let source = "{\n  // a comment\n  \"todo\": 1\n}\n";
    let src = make_src("tsconfig.jsonc", Language::Jsonc, source);

    let diags = engine.lint(&src, &cfg).unwrap();
    assert!(
        diags.iter().any(|d| d.code.as_deref() == Some("no-todo-key")),
        "a `language: json` rule must fire on a .jsonc file; got: {diags:?}"
    );
}

/// The **built-in pack** must pass its own `*-test.yml` corpora.
///
/// `shipped_rule_library_passes_its_tests` covers the repo's top-level
/// `rules/` directory; the pack embedded in the binary had no equivalent, so
/// its 26 rules were verified only when someone remembered to run
/// `poly rules test crates/poly-core/src/engines/astgrep/builtin/` by hand.
/// That is the pack a default `poly lint` actually runs, and a refactor of how
/// its rules resolve — extracting shared `utils:` into global rules, say —
/// could change what they match with nothing failing.
///
/// The pack is loaded here as an ordinary rule directory, which is the same
/// path `poly rules test` takes, so this also pins that the pack's YAML stays
/// loadable by the user-rule loader rather than depending on `builtin_pack`'s
/// own parse.
#[test]
fn builtin_pack_passes_its_own_test_corpora() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/src/engines/astgrep/builtin").to_string();
    let report = poly_core::engines::astgrep::test::run_tests(&[root]).unwrap();

    assert!(
        report.missing_rule_ids.is_empty(),
        "pack test files name unknown rule ids: {:?}",
        report.missing_rule_ids
    );
    assert!(
        report.passed() > 0,
        "expected the built-in pack to run some snippet checks"
    );
    assert_eq!(
        report.failed(),
        0,
        "built-in pack has failing snippets: {:?}",
        report
            .outcomes
            .iter()
            .filter(|o| !o.passed)
            .map(|o| (&o.rule_id, o.kind, o.index))
            .collect::<Vec<_>>()
    );
}
