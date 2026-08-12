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
    // `code_only = false` exercises the underlying strip-every-comment wrapper
    // (preservation rules, spans, delete-edits); the default code-only filtering
    // is covered by the `code_only_*` tests below.
    let engine = UncommentEngine;
    let diagnostics = engine
        .lint(
            &src("main.rs", Language::Rust, RUST_SAMPLE),
            &cfg("enabled = true\ncode_only = false"),
        )
        .unwrap();

    // Only the two plain comments are removable; TODO, ~keep and the doc comment
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

    assert_eq!(diagnostics[0].span.unwrap().start_line, 1);
    assert_eq!(diagnostics[1].span.unwrap().start_line, 3);
}

#[test]
fn fix_strips_comments() {
    let engine = UncommentEngine;
    let diagnostics = engine
        .lint(
            &src("main.rs", Language::Rust, RUST_SAMPLE),
            &cfg("enabled = true\ncode_only = false"),
        )
        .unwrap();
    let stripped = apply_deletions(RUST_SAMPLE, &diagnostics);

    assert!(!stripped.contains("standalone removable"));
    assert!(!stripped.contains("trailing removable"));
    assert!(!stripped.starts_with("//"), "leading comment line fully removed");
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
        .lint(
            &src("module.py", Language::Python, source),
            &cfg("enabled = true\ncode_only = false"),
        )
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
    // The TODO stands alone (no `~keep` on an adjacent line): uncomment's
    // block-level `~keep` preserves a whole contiguous comment block when any
    // line in it carries `~keep`, so a TODO next to `~keep` stays regardless of
    // `remove_todos`. Isolating it keeps this test about `remove_todos` alone.
    const TODO_SAMPLE: &str = "fn main() {\n    // TODO: strip me\n    let z = 3;\n}\n";
    let engine = UncommentEngine;
    let diagnostics = engine
        .lint(
            &src("main.rs", Language::Rust, TODO_SAMPLE),
            &cfg("enabled = true\ncode_only = false\nremove_todos = true"),
        )
        .unwrap();
    let previews: Vec<Option<&str>> = diagnostics
        .iter()
        .map(|diagnostic| diagnostic.description.as_deref())
        .collect();
    assert!(
        previews.contains(&Some("// TODO: strip me")),
        "remove_todos = true makes the TODO removable, got {previews:?}"
    );
}

#[test]
fn unsupported_language_is_noop() {
    let engine = UncommentEngine;
    let diagnostics = engine
        .lint(
            &src("data.unknownext", Language::Rust, "// whatever\n"),
            &cfg("enabled = true"),
        )
        .unwrap();
    assert!(diagnostics.is_empty());
}

/// Collect the previews of every diagnostic the engine reports for `content`
/// with the default (code-only) configuration.
fn code_only_previews(path: &str, language: Language, content: &str) -> Vec<String> {
    UncommentEngine
        .lint(&src(path, language, content), &cfg("enabled = true"))
        .unwrap()
        .into_iter()
        .filter_map(|diagnostic| diagnostic.description)
        .collect()
}

#[test]
fn code_only_ignores_alef_hash_header() {
    let toml = "# This file is auto-generated by alef. DO NOT EDIT.\n\
        # alef:hash:cba947bdd989e2d5af4d9d0d92fa7d3024ad0b0bd1184fb400bee8d671468c90\n\
        # Re-generate with: alef scaffold\n\n\
        [build]\nincremental = true\n";
    assert!(
        code_only_previews("config.toml", Language::Toml, toml).is_empty(),
        "machine-generated headers and regenerate directives must not be flagged"
    );
}

#[test]
fn code_only_ignores_multiline_english_note() {
    let toml = "[build]\nincremental = true\n\n\
        # Required for PyO3 / ext-php-rs cdylibs: Python and Zend C-API symbols are\n\
        # resolved at runtime when the host loads the extension, not at link time.\n\
        # macOS ld is strict and rejects unresolved symbols by default.\n\
        [net]\ngit-fetch-with-cli = true\n";
    assert!(
        code_only_previews("config.toml", Language::Toml, toml).is_empty(),
        "a multi-line English NOTE block is prose, not commented-out code"
    );
}

#[test]
fn code_only_ignores_key_value_directive() {
    let toml = "# indent_size = 4\n[build]\nincremental = true\n";
    assert!(
        code_only_previews("config.toml", Language::Toml, toml).is_empty(),
        "a `# key = value` directive comment must not be flagged"
    );
}

#[test]
fn code_only_still_flags_commented_out_rust_code() {
    let rust = "fn main() {\n    // let x = foo();\n    let y = 2;\n}\n";
    let previews = code_only_previews("main.rs", Language::Rust, rust);
    assert_eq!(
        previews,
        vec!["// let x = foo();".to_owned()],
        "genuine commented-out code is still reported"
    );
}

#[test]
fn code_only_still_flags_commented_out_python_code() {
    let python = "def f():\n    # print(\"debug\")\n    return 1\n";
    let previews = code_only_previews("module.py", Language::Python, python);
    assert_eq!(
        previews,
        vec!["# print(\"debug\")".to_owned()],
        "a commented-out Python call is still reported"
    );
}

/// A sentence continued across lines ends mid-clause, often on `;`. Judging
/// prose by its closing character alone deleted this line from a Helm values
/// file, leaving the surrounding paragraph reading as valid English with its
/// explanation silently gone — data loss that survives review.
#[test]
fn code_only_ignores_prose_continued_across_lines() {
    let yaml = concat!(
        "engine:\n",
        "    # Default backend when the per-request override isn't set. Native is\n",
        "    # lighter (in-process, no Chrome dep) and is our recommended default.\n",
        "    # Chromiumoxide engine still gets built because browserEndpoint is set;\n",
        "    # both engines coexist on every pod.\n",
        "    browserBackend: \"native\"\n",
    );

    let diagnostics = UncommentEngine
        .lint(&src("values.yaml", Language::Yaml, yaml), &cfg("enabled = true\n"))
        .expect("lint");

    assert!(
        diagnostics.is_empty(),
        "prose ending in ';' must not be reported as removable code"
    );
}

/// The guard must not stop poly noticing genuinely commented-out code.
#[test]
fn code_only_still_flags_commented_out_code_ending_in_semicolon() {
    let rust = "fn main() {\n    // let unused = compute(a, b);\n}\n";

    let diagnostics = UncommentEngine
        .lint(&src("main.rs", Language::Rust, rust), &cfg("enabled = true\n"))
        .expect("lint");

    assert!(!diagnostics.is_empty(), "commented-out code must still be reported");
}
