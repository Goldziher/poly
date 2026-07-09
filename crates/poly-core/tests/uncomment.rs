//! Fixtures for the opt-in `uncomment` comment-removal lint backend.
//!
//! - `disabled_by_default` — with no `enabled` option the engine produces no
//!   findings (it is opt-in).
//! - `reports_removable_comments_as_warnings` — a Rust sample with removable and
//!   preserved comments asserts the expected [`Diagnostic`]s (Warning severity,
//!   one delete-edit each; TODO / `~keep` / doc comments preserved).
//! - `fix_strips_comments` — applying the diagnostics' edits removes exactly the
//!   removable comments and keeps the preserved ones.
//! - `python_docstrings_preserved` — a Python docstring survives by default while
//!   a plain `#` comment is flagged.
//! - `remove_todos_option` — `remove_todos = true` makes the TODO removable.
//! - `unsupported_language_is_noop` — an unknown extension yields no findings and
//!   no error.

use poly_core::{
    Diagnostic, Language, Severity,
    config::{EngineConfig, GlobalDefaults},
    engine::{Engine, SourceFile},
    engines::uncomment::UncommentEngine,
};

/// Build an [`EngineConfig`] whose `options` come from a TOML snippet (the merged
/// `[lint.uncomment]` table the runner would hand the engine).
fn cfg(options_toml: &str) -> EngineConfig {
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 4,
        options: toml::from_str(options_toml).expect("valid options toml"),
    }
}

fn src(path: &str, language: Language, content: &str) -> SourceFile {
    SourceFile {
        path: path.into(),
        language,
        content: content.into(),
    }
}

/// Apply every diagnostic's delete-edit (highest offset first so earlier offsets
/// stay valid) to reproduce what `poly lint --fix` would write.
fn apply_deletions(source: &str, diagnostics: &[Diagnostic]) -> String {
    let mut ranges: Vec<(usize, usize)> = diagnostics
        .iter()
        .flat_map(|diagnostic| diagnostic.fix.iter())
        .map(|edit| (edit.start_byte, edit.end_byte))
        .collect();
    ranges.sort_by_key(|range| std::cmp::Reverse(range.0));
    let mut output = source.to_owned();
    for (start, end) in ranges {
        output.replace_range(start..end, "");
    }
    output
}

const RUST_SAMPLE: &str = "// standalone removable\n\
fn main() {\n\
    let x = 1; // trailing removable\n\
    // TODO: keep me\n\
    // ~keep pinned\n\
    /// doc comment\n\
    let y = 2;\n\
}\n";

#[test]
fn disabled_by_default() {
    let engine = UncommentEngine;
    let diagnostics = engine
        .lint(&src("main.rs", Language::Rust, RUST_SAMPLE), &cfg(""))
        .unwrap();
    assert!(
        diagnostics.is_empty(),
        "engine must be a no-op until [lint.uncomment] enabled = true"
    );
}

#[test]
fn reports_removable_comments_as_warnings() {
    let engine = UncommentEngine;
    let diagnostics = engine
        .lint(&src("main.rs", Language::Rust, RUST_SAMPLE), &cfg("enabled = true"))
        .unwrap();

    // Only the two plain comments are removable; TODO, ~keep and the doc comment
    // are preserved by the default rules.
    let previews: Vec<Option<&str>> = diagnostics
        .iter()
        .map(|diagnostic| diagnostic.description.as_deref())
        .collect();
    assert_eq!(
        previews,
        vec![Some("// standalone removable"), Some("// trailing removable")]
    );

    for diagnostic in &diagnostics {
        assert_eq!(diagnostic.engine, "uncomment");
        assert_eq!(diagnostic.severity, Severity::Warning);
        assert_eq!(diagnostic.code.as_deref(), Some("comment"));
        assert_eq!(
            diagnostic.fix.len(),
            1,
            "each removable comment carries one delete-edit"
        );
        assert_eq!(diagnostic.fix[0].replacement, "");
    }

    // The standalone comment is on line 1; the trailing comment on line 3.
    assert_eq!(diagnostics[0].span.unwrap().start_line, 1);
    assert_eq!(diagnostics[1].span.unwrap().start_line, 3);
}

#[test]
fn fix_strips_comments() {
    let engine = UncommentEngine;
    let diagnostics = engine
        .lint(&src("main.rs", Language::Rust, RUST_SAMPLE), &cfg("enabled = true"))
        .unwrap();
    let stripped = apply_deletions(RUST_SAMPLE, &diagnostics);

    // Removable comments are gone; the standalone one took its whole line with it.
    assert!(!stripped.contains("standalone removable"));
    assert!(!stripped.contains("trailing removable"));
    assert!(!stripped.starts_with("//"), "leading comment line fully removed");
    // Preserved comments and code remain.
    assert!(stripped.contains("// TODO: keep me"));
    assert!(stripped.contains("// ~keep pinned"));
    assert!(stripped.contains("/// doc comment"));
    assert!(stripped.contains("let x = 1;"));
    assert!(stripped.contains("let y = 2;"));
}

#[test]
fn python_docstrings_preserved() {
    let engine = UncommentEngine;
    let source = "def f():\n    \"\"\"docstring\"\"\"\n    # remove me\n    return 1\n";
    let diagnostics = engine
        .lint(&src("module.py", Language::Python, source), &cfg("enabled = true"))
        .unwrap();

    let previews: Vec<Option<&str>> = diagnostics
        .iter()
        .map(|diagnostic| diagnostic.description.as_deref())
        .collect();
    assert_eq!(
        previews,
        vec![Some("# remove me")],
        "the docstring is preserved by default; only the plain comment is removable"
    );
}

#[test]
fn remove_todos_option() {
    let engine = UncommentEngine;
    let diagnostics = engine
        .lint(
            &src("main.rs", Language::Rust, RUST_SAMPLE),
            &cfg("enabled = true\nremove_todos = true"),
        )
        .unwrap();
    let previews: Vec<Option<&str>> = diagnostics
        .iter()
        .map(|diagnostic| diagnostic.description.as_deref())
        .collect();
    assert!(
        previews.contains(&Some("// TODO: keep me")),
        "remove_todos = true makes the TODO removable, got {previews:?}"
    );
}

#[test]
fn unsupported_language_is_noop() {
    let engine = UncommentEngine;
    // An extension the uncomment registry does not know: skipped, never an error.
    // The Language variant is irrelevant here — the engine detects language from
    // the path's extension, which the uncomment registry does not recognize.
    let diagnostics = engine
        .lint(
            &src("data.unknownext", Language::Rust, "// whatever\n"),
            &cfg("enabled = true"),
        )
        .unwrap();
    assert!(diagnostics.is_empty());
}
