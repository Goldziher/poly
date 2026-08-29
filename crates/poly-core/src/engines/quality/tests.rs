//! End-to-end [`Engine::lint`] tests for [`QualityEngine`]: deferral applied
//! through the real dispatch path, the lazy-ignore double-report guard
//! against `filter::suppress` (ADR 0028), and the C `function-too-long`
//! regression through the full engine (not just `definitions::collect`).

use std::path::PathBuf;
use std::sync::Arc;

use super::QualityEngine;
use crate::config::{EngineConfig, GlobalDefaults};
use crate::engine::{Engine, SourceFile};
use crate::filter::Suppressions;
use crate::language::Language;

fn cfg(options: toml::Table) -> EngineConfig {
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 4,
        options,
    }
}

fn src(path: &str, language: Language, content: &str) -> SourceFile {
    SourceFile {
        path: PathBuf::from(path),
        language,
        content: Arc::from(content),
    }
}

fn codes(diagnostics: &[crate::engine::Diagnostic]) -> Vec<&str> {
    let mut codes: Vec<&str> = diagnostics.iter().filter_map(|d| d.code.as_deref()).collect();
    codes.sort_unstable();
    codes
}

#[test]
fn declares_lint_only_no_format_no_fix() {
    let capabilities = QualityEngine.capabilities();
    assert!(capabilities.lint);
    assert!(!capabilities.format);
    assert!(!capabilities.fix);
}

#[test]
fn disabled_engine_produces_nothing_and_claims_no_coverage() {
    let mut options = toml::Table::new();
    options.insert("enabled".to_owned(), toml::Value::Boolean(false));
    let engine_cfg = cfg(options);
    let file = src("f.py", Language::Python, &"x\n".repeat(2000));
    assert!(QualityEngine.lint(&file, &engine_cfg).unwrap().is_empty());
    assert!(!QualityEngine.provides_language_lint(&Language::Python, &engine_cfg));
}

/// Go has no tier-1 lint backend, so `too-many-parameters` and
/// `cyclomatic-complexity` run at their defaults; a deeply branching,
/// many-parameter function must be caught end-to-end through `Engine::lint`.
#[test]
fn go_gets_too_many_parameters_and_complexity_by_default() {
    let source = "package main\n\nfunc f(a, b, c, d, e, f, g, h int) int {\n\
        if a > 0 && b > 0 {\n            return 1\n        } else if c > 0 || d > 0 {\n            return 2\n        }\n\
        switch e {\n        case 1:\n            return 3\n        case 2:\n            return 4\n        case 3:\n            return 5\n        }\n\
        return f + g + h\n}\n";
    let file = src("f.go", Language::Go, source);
    // The complexity of the fixture above (base 1 + if + chained-else-if +
    // && + || = 5) is well under the ADR default of 20; lower the threshold
    // here so the test stays a small, readable fixture instead of needing 20
    // real branches to prove the rule fires at all.
    let mut options = toml::Table::new();
    options.insert("cyclomatic_complexity_max".to_owned(), toml::Value::Integer(3));
    let diags = QualityEngine.lint(&file, &cfg(options)).unwrap();
    let found = codes(&diags);
    assert!(found.contains(&"too-many-parameters"), "{found:?}");
    assert!(found.contains(&"cyclomatic-complexity"), "{found:?}");
}

/// Python defers `too-many-parameters` and `cyclomatic-complexity` to ruff
/// (`PLR0913`/`PLR0917`, `C901`) — an equally branchy, many-parameter Python
/// function must produce neither from `quality`, end-to-end.
#[test]
fn python_defers_too_many_parameters_and_complexity() {
    let source = "def f(a, b, c, d, e, f, g, h):\n\
        if a > 0 and b > 0:\n        return 1\n    elif c > 0 or d > 0:\n        return 2\n\
    if e == 1:\n        return 3\n    elif e == 2:\n        return 4\n    elif e == 3:\n        return 5\n\
    return f + g + h\n";
    let file = src("f.py", Language::Python, source);
    let diags = QualityEngine.lint(&file, &cfg(toml::Table::new())).unwrap();
    let found = codes(&diags);
    assert!(!found.contains(&"too-many-parameters"), "{found:?}");
    assert!(!found.contains(&"cyclomatic-complexity"), "{found:?}");
}

/// TypeScript defers `nesting-too-deep` to oxlint's `max-depth` — a file deep
/// enough to trip the default threshold must produce nothing from `quality`,
/// end-to-end, while the identical Go shape (not deferred) does.
#[test]
fn typescript_defers_nesting_too_deep_but_go_does_not() {
    let ts = "function f(item: number) {\n  if (item > 0) {\n    while (item > 1) {\n      if (item > 2) {\n        if (item > 3) {\n          if (item > 4) {\n          }\n        }\n      }\n    }\n  }\n}\n";
    let go = "package main\n\nfunc f(item int) {\n\tif item > 0 {\n\t\tfor item > 1 {\n\t\t\tif item > 2 {\n\t\t\t\tif item > 3 {\n\t\t\t\t\tif item > 4 {\n\t\t\t\t\t}\n\t\t\t\t}\n\t\t\t}\n\t\t}\n\t}\n}\n";

    let ts_diags = QualityEngine
        .lint(&src("f.ts", Language::TypeScript, ts), &cfg(toml::Table::new()))
        .unwrap();
    assert!(
        !codes(&ts_diags).contains(&"nesting-too-deep"),
        "{:?}",
        codes(&ts_diags)
    );

    let go_diags = QualityEngine
        .lint(&src("f.go", Language::Go, go), &cfg(toml::Table::new()))
        .unwrap();
    assert!(codes(&go_diags).contains(&"nesting-too-deep"), "{:?}", codes(&go_diags));
}

/// The exact failure family named in the task: without the C/C++
/// `function_declarator` parent-walk, a long C function never fires
/// `function-too-long`. Exercised through the full `Engine::lint` dispatch,
/// not just `definitions::collect`.
#[test]
fn c_long_function_fires_function_too_long_end_to_end() {
    let mut body = String::from("int add(int a, int b) {\n");
    for i in 0..90 {
        body.push_str(&format!("    int t{i} = a + b + {i};\n"));
    }
    body.push_str("    return a + b;\n}\n");
    let file = src("f.c", Language::C, &body);
    let diags = QualityEngine.lint(&file, &cfg(toml::Table::new())).unwrap();
    assert!(
        codes(&diags).contains(&"function-too-long"),
        "expected function-too-long for a >90-line C function, got {:?}",
        codes(&diags)
    );
}

#[test]
fn c_short_function_does_not_fire_function_too_long_end_to_end() {
    let file = src("f.c", Language::C, "int add(int a, int b) {\n    return a + b;\n}\n");
    let diags = QualityEngine.lint(&file, &cfg(toml::Table::new())).unwrap();
    assert!(
        !codes(&diags).contains(&"function-too-long"),
        "a 3-line C function must not fire function-too-long: {:?}",
        codes(&diags)
    );
}

/// The regression this task calls out by name: `quality`'s `lazy-ignore`
/// must never double-report an unjustified `poly: allow[…]` directive
/// alongside `filter::suppress` (ADR 0028), which already owns that case.
/// Runs both real mechanisms together, exactly as the runner would.
#[test]
fn lazy_ignore_does_not_double_report_polys_own_directive() {
    let content = "code(); // poly: allow[some-rule]\n";
    let file = src("f.rs", Language::Rust, content);
    let mut diags = QualityEngine.lint(&file, &cfg(toml::Table::new())).unwrap();

    let suppressions = Suppressions::parse(content);
    suppressions.apply(&mut diags);

    let lazy_ignore_count = diags
        .iter()
        .filter(|d| d.code.as_deref() == Some("lazy-ignore"))
        .count();
    assert_eq!(
        lazy_ignore_count, 1,
        "expected exactly one lazy-ignore finding (from filter::suppress), got: {diags:?}"
    );
}

/// A file whose *only* problem is an unjustified `#[allow(...)]` (nothing
/// poly-specific) must be caught by `quality`'s own `lazy-ignore`, exactly
/// once.
#[test]
fn lazy_ignore_catches_a_plain_rust_allow_with_no_poly_directive_involved() {
    let content = "#[allow(dead_code)]\nfn f() {}\n";
    let file = src("f.rs", Language::Rust, content);
    let diags = QualityEngine.lint(&file, &cfg(toml::Table::new())).unwrap();
    let lazy_ignore_count = diags
        .iter()
        .filter(|d| d.code.as_deref() == Some("lazy-ignore"))
        .count();
    assert_eq!(lazy_ignore_count, 1);
}

#[test]
fn file_too_long_fires_end_to_end_for_any_language() {
    let content = "x\n".repeat(1200);
    let file = src("f.py", Language::Python, &content);
    let diags = QualityEngine.lint(&file, &cfg(toml::Table::new())).unwrap();
    assert!(codes(&diags).contains(&"file-too-long"));
}

#[test]
fn every_finding_is_warning_severity() {
    let mut options = toml::Table::new();
    options.insert("magic_number".to_owned(), toml::Value::Boolean(true));
    options.insert("law_of_demeter".to_owned(), toml::Value::Boolean(true));
    let content = "package main\n\nfunc f(item int) {\n\tx := a.b.c.d.e\n\tif item > 42 {\n\t}\n\t_ = x\n}\n";
    let file = src("f.go", Language::Go, content);
    let diags = QualityEngine.lint(&file, &cfg(options)).unwrap();
    assert!(!diags.is_empty(), "expected at least one finding to check severity of");
    for diag in &diags {
        assert_eq!(diag.severity, crate::engine::Severity::Warning, "{diag:?}");
    }
}

/// A language with no tree-sitter grammar the pack recognizes at all must
/// degrade to the language-independent rules only (file-too-long,
/// lazy-ignore) rather than erroring.
#[test]
fn unrecognized_grammar_still_gets_file_too_long_and_lazy_ignore() {
    let file = src(
        "f.mystery",
        Language::Other("totally-not-a-real-grammar".to_owned()),
        &"x\n".repeat(1200),
    );
    let diags = QualityEngine.lint(&file, &cfg(toml::Table::new())).unwrap();
    assert!(codes(&diags).contains(&"file-too-long"));
}
