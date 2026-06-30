//! Insta snapshot fixtures for the malva CSS / SCSS / Less backend.
//!
//! malva is format-only. Two fixtures:
//! - `lint_no_diagnostics` — confirms the engine always returns an empty
//!   diagnostic list (format-only; no lint capability).
//! - `format_css_snapshot` — a known-unformatted CSS file asserts the exact
//!   output produced by malva with polylint's opinionated defaults
//!   (print_width=120, indent_width=2).
//! - `format_scss_snapshot` — same for SCSS with nested rules.

use polylint_core::{
    Language,
    config::{EngineConfig, GlobalDefaults},
    engine::{Engine, FormatOutput, SourceFile},
    engines::malva::MalvaEngine,
};

fn engine_cfg() -> EngineConfig {
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 2,
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

// ---------------------------------------------------------------------------
// malva is format-only — lint must always return empty diagnostics.
// ---------------------------------------------------------------------------

/// CSS file with missing spaces; malva should produce no diagnostics (it's a
/// formatter, not a linter).
const KNOWN_BAD_CSS: &str = ".foo{color:red;background:blue;}";

#[test]
fn lint_no_diagnostics() {
    let engine = MalvaEngine;
    let src = make_src("bad.css", Language::Css, KNOWN_BAD_CSS);
    let diags = engine.lint(&src, &engine_cfg()).unwrap();
    insta::assert_debug_snapshot!("lint_no_diagnostics", diags);
}

// ---------------------------------------------------------------------------
// Known-unformatted CSS: compact spacing, missing spaces → canonical output.
// Also exercises print_width=120 (polylint opinionated default).
// ---------------------------------------------------------------------------

/// CSS with compact syntax that malva must expand into canonical multi-line form.
const KNOWN_UNFORMATTED_CSS: &str = "\
.container{display:flex;flex-direction:column;align-items:center;justify-content:space-between;padding:0 20px;margin:0 auto;}
.button { background-color:#4CAF50;color:white;padding:  15px 32px;border: none;cursor: pointer; }
";

#[test]
fn format_css_snapshot() {
    let engine = MalvaEngine;
    let src = make_src("unformatted.css", Language::Css, KNOWN_UNFORMATTED_CSS);
    let result = engine.format(&src, &engine_cfg()).unwrap();

    let formatted = match result {
        FormatOutput::Formatted(s) => s,
        FormatOutput::Unchanged => KNOWN_UNFORMATTED_CSS.to_string(),
    };

    insta::assert_snapshot!("format_css_output", formatted);
}

// ---------------------------------------------------------------------------
// Known-unformatted SCSS: nested rules + variable, exercises indent_width=2.
// ---------------------------------------------------------------------------

/// SCSS with nested selectors and a variable that malva should format to
/// canonical 2-space-indented output.
const KNOWN_UNFORMATTED_SCSS: &str = "\
$primary: #333333;
$secondary:   #666;
.nav{display:flex;background:$primary;
.link{color:white;padding:8px 16px;
&:hover{color:$secondary;text-decoration:underline;}}}
";

#[test]
fn format_scss_snapshot() {
    let engine = MalvaEngine;
    let src = make_src("unformatted.scss", Language::Scss, KNOWN_UNFORMATTED_SCSS);
    let result = engine.format(&src, &engine_cfg()).unwrap();

    let formatted = match result {
        FormatOutput::Formatted(s) => s,
        FormatOutput::Unchanged => KNOWN_UNFORMATTED_SCSS.to_string(),
    };

    insta::assert_snapshot!("format_scss_output", formatted);
}

// ---------------------------------------------------------------------------
// Already-formatted input must round-trip as Unchanged.
// ---------------------------------------------------------------------------

#[test]
fn format_unchanged_for_canonical_css() {
    let engine = MalvaEngine;
    // Canonical malva output: two-space indent, space after selector brace.
    let canonical = ".foo {\n  color: red;\n}\n";
    let src = make_src("clean.css", Language::Css, canonical);
    let result = engine.format(&src, &engine_cfg()).unwrap();
    assert!(
        matches!(result, FormatOutput::Unchanged),
        "canonical CSS must round-trip as Unchanged"
    );
}

/// A malva LanguageOptions field set via `[fmt.css.malva]` reaches the
/// formatter: `hex-case = "upper"` uppercases hex colors.
#[test]
fn format_honors_language_option() {
    let engine = MalvaEngine;
    let src = make_src("hex.css", Language::Css, "a {\n  color: #fff;\n}\n");
    let mut options = toml::Table::new();
    options.insert("hex_case".to_string(), toml::Value::String("upper".into()));
    let cfg = EngineConfig {
        options,
        ..engine_cfg()
    };
    let FormatOutput::Formatted(out) = engine.format(&src, &cfg).unwrap() else {
        panic!("`hex_case = upper` should uppercase the hex color");
    };
    assert!(
        out.contains("#FFF"),
        "[fmt.css.malva] hex_case must reach malva; got: {out}"
    );
}
