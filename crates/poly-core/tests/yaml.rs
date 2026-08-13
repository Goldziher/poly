//! Insta snapshot fixtures for the YAML backend.
//!
//! - `known_bad_diagnostics` — a YAML file with an unclosed flow sequence
//!   asserts the expected parse-error [`Diagnostic`] (`syntax`).
//! - `known_unformatted_output` — a YAML file with trailing whitespace and a
//!   missing final newline asserts the normalized output.

use poly_core::{
    Language,
    config::{EngineConfig, GlobalDefaults},
    engine::{Engine, FormatOutput, SourceFile},
    engines::yaml::YamlEngine,
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
        language: Language::Yaml,
        content: content.into(),
    }
}

/// Unclosed `[` — saphyr returns a ScanError at end-of-file.
const KNOWN_BAD: &str = "items: [1, 2, 3\nother: value\n";

#[test]
fn known_bad_diagnostics() {
    let engine = YamlEngine;
    let src = make_src("known_bad.yaml", KNOWN_BAD);
    let diags = engine.lint(&src, &engine_cfg()).unwrap();

    assert!(!diags.is_empty(), "expected a parse-error diagnostic");
    let summary: Vec<_> = diags
        .iter()
        .map(|d| {
            (
                d.engine.as_str(),
                d.code.as_deref().unwrap_or(""),
                d.severity,
                d.span.as_ref().map(|s| (s.start_line, s.start_col)),
            )
        })
        .collect();
    insta::assert_debug_snapshot!("known_bad_diagnostics", summary);
}

#[test]
fn valid_yaml_has_no_diagnostics() {
    let engine = YamlEngine;
    let src = make_src(
        "ok.yaml",
        "name: example\nversion: \"1.0\"\nitems:\n  - alpha\n  - beta\n",
    );
    let diags = engine.lint(&src, &engine_cfg()).unwrap();
    assert!(diags.is_empty(), "got: {diags:?}");
}

/// Trailing spaces on lines 1 and 3, no final newline.
const KNOWN_UNFORMATTED: &str = "name: example   \nversion: 1.0\ndescription: test  ";

#[test]
fn known_unformatted_output() {
    let engine = YamlEngine;
    let src = make_src("unformatted.yaml", KNOWN_UNFORMATTED);
    match engine.format(&src, &engine_cfg()).unwrap() {
        FormatOutput::Formatted(text) => {
            insta::assert_snapshot!("known_unformatted_output", text);
        }
        FormatOutput::Unchanged => panic!("expected Formatted, got Unchanged"),
    }
}

#[test]
fn already_formatted_returns_unchanged() {
    let engine = YamlEngine;
    let src = make_src("clean.yaml", "name: example\nversion: 1.0\n");
    let result = engine.format(&src, &engine_cfg()).unwrap();
    assert!(
        matches!(result, FormatOutput::Unchanged),
        "expected Unchanged for already-clean YAML"
    );
}

/// Extra spaces after `:` on the first key and after `-` on the last list
/// item.  No trailing whitespace so prek hooks leave this literal alone.
const STRUCTURAL_UNFORMATTED: &str = "a:    1\nb:\n  - x\n  -   y\n";

#[test]
fn structural_reformat_canonicalizes_spacing() {
    let engine = YamlEngine;
    let src = make_src("structural.yaml", STRUCTURAL_UNFORMATTED);
    match engine.format(&src, &engine_cfg()).unwrap() {
        FormatOutput::Formatted(text) => {
            assert_ne!(
                text, STRUCTURAL_UNFORMATTED,
                "formatted output should differ from input"
            );
            insta::assert_snapshot!("structural_reformat", text);
        }
        FormatOutput::Unchanged => panic!("expected Formatted, got Unchanged"),
    }
}

/// A `pretty_yaml` LanguageOptions field set via `[fmt.yaml.yaml]` reaches the
/// formatter: `quotes = "prefer-single"` flips a double-quoted scalar to single
/// quotes (default is prefer-double).
#[test]
fn format_honors_language_option() {
    let engine = YamlEngine;
    let src = make_src("q.yaml", "key: \"value\"\n");
    let mut options = toml::Table::new();
    options.insert("quotes".to_string(), toml::Value::String("prefer-single".into()));
    let cfg = EngineConfig {
        options,
        ..engine_cfg()
    };
    let FormatOutput::Formatted(out) = engine.format(&src, &cfg).unwrap() else {
        panic!("`quotes = prefer-single` should reformat the double-quoted scalar");
    };
    assert!(
        out.contains("'value'") && !out.contains("\"value\""),
        "[fmt.yaml.yaml] quotes must reach pretty_yaml; got: {out}"
    );
}

// --- Go/Helm template detection stays byte-for-byte strict for YAML ----------
//
// The Markdown backend gained a code-block carve-out (template syntax inside a
// fenced/indented block or an inline span is documentation, not a template).
// YAML has no such construct, so its detection must be unchanged: anything that
// looks like a Markdown fence or backtick span in a YAML file is just a scalar.

/// The exact reason string the backends report for a Go/Helm template file.
const TEMPLATE_SKIP: &str = "Go/Helm template syntax";

/// A real Helm chart template — must always be skipped.
const HELM_TEMPLATE: &str = "\
apiVersion: apps/v1
kind: Deployment
metadata:
  name: {{ include \"chart.fullname\" . }}
spec:
  replicas: {{ .Values.replicaCount }}
{{- if .Values.enabled }}
  template:
    metadata:
      labels:
{{ toYaml .Values.labels | nindent 8 }}
{{- end }}
";

#[test]
fn helm_yaml_is_still_skipped() {
    let engine = YamlEngine;
    let src = make_src("templates/deployment.yaml", HELM_TEMPLATE);
    assert_eq!(engine.skip_reason(&src), Some(TEMPLATE_SKIP));
    assert_eq!(engine.lint(&src, &engine_cfg()).unwrap().len(), 0);
    assert!(matches!(
        engine.format(&src, &engine_cfg()).unwrap(),
        FormatOutput::Unchanged
    ));
}

#[test]
fn yaml_template_inside_backticks_or_fences_is_still_skipped() {
    let engine = YamlEngine;
    let fenced = make_src("notes.yaml", "notes: |\n  ```\n  image: {{ .Values.image }}\n  ```\n");
    assert_eq!(
        engine.skip_reason(&fenced),
        Some(TEMPLATE_SKIP),
        "a Markdown fence inside YAML is a plain scalar, not a code block"
    );

    let inline = make_src("notes.yaml", "note: \"see `{{ .Values.image }}`\"\n");
    assert_eq!(
        engine.skip_reason(&inline),
        Some(TEMPLATE_SKIP),
        "backticks inside YAML are plain characters, not an inline code span"
    );
}

#[test]
fn github_actions_expressions_are_still_not_templates() {
    let engine = YamlEngine;
    let src = make_src(
        ".github/workflows/ci.yaml",
        "jobs:\n  build:\n    if: ${{ github.event_name == 'push' }}\n",
    );
    assert_eq!(engine.skip_reason(&src), None);
}
