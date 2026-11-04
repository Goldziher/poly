//! Pipeline tests that need the web family's engines: Markdown autofix (rumdl), the real-output JSON
//! schema (taplo) and the Go/Helm template skip (rumdl, yaml). The core pipeline tests live in
//! `crates/poly-core/tests/pipeline.rs`.

use std::fs;

use poly_core::report::report_lint_json;
use poly_core::{Config, RunOptions};

mod common;

fn write(dir: &std::path::Path, name: &str, content: &str) -> std::path::PathBuf {
    let p = dir.join(name);
    fs::write(&p, content).unwrap();
    p
}

#[test]
fn lint_fix_applies_autofixes_and_dry_run_does_not() {
    common::register();
    let before = "#Heading\n\nBody text.\n";
    let after = "# Heading\n\nBody text.\n";
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "notes.md", before);
    let cfg = Config::default();
    let opts = RunOptions {
        no_cache: true,
        jobs: Some(1),
        exclude: Vec::new(),
        force_exclude: false,
        fix_generated: false,
        generated: None,
        explicit_config: true,
        config_resolver: None,
        externally_linted_languages: Vec::new(),
        only: Vec::new(),
        skip: Vec::new(),
        ..RunOptions::default()
    };

    poly_core::lint(&[dir.path().to_path_buf()], &cfg, &opts, false, false).unwrap();
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        before,
        "dry run must not modify files"
    );

    poly_core::lint(&[dir.path().to_path_buf()], &cfg, &opts, true, false).unwrap();
    let fixed = fs::read_to_string(&path).unwrap();
    assert_eq!(
        fixed, after,
        "the MD018 heading-space autofix should be applied in place"
    );
}

/// Real-output schema check: run a real backend end-to-end, render with
/// `report_lint_json`, and verify the resulting JSON conforms to the
/// `LintResult` envelope schema. Key assertions:
///
/// - Top-level is an array of `{ path, diagnostics }` objects.
/// - Each diagnostic has the required string fields `engine`, `severity`,
///   `title` (non-empty).
/// - **Optional fields that are `None` are omitted** — no `"description"` key,
///   no `"url"` key, no `"code"` key when `None`. This proves real backend
///   output obeys the `#[serde(skip_serializing_if = "Option::is_none")]`
///   contract, not just the synthetic report snapshots.
/// - `"fix"` is absent when the slice is empty (`skip_serializing_if =
///   "Vec::is_empty"`).
/// - `"metadata"` is absent when the map is empty.
///
/// Uses a TOML duplicate-key fixture (taplo) because taplo always sets both
/// `code` and `span` on real findings — a reliable, deterministic canary.
#[test]
fn lint_json_output_schema_conforms_to_diagnostic_contract() {
    common::register();
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "schema_check.toml",
        "name = \"polylint\"\nname = \"duplicate\"\n",
    );
    let cfg = Config::default();
    let opts = RunOptions {
        no_cache: true,
        jobs: Some(1),
        exclude: Vec::new(),
        force_exclude: false,
        fix_generated: false,
        generated: None,
        explicit_config: true,
        config_resolver: None,
        externally_linted_languages: Vec::new(),
        only: Vec::new(),
        skip: Vec::new(),
        ..RunOptions::default()
    };

    let results = poly_core::lint(&[dir.path().to_path_buf()], &cfg, &opts, false, false).unwrap();

    assert!(
        !results.is_empty(),
        "expected diagnostics from the duplicate-key TOML fixture"
    );

    let json = report_lint_json(&results).expect("report_lint_json must render");
    let value: serde_json::Value = serde_json::from_str(&json).expect("report_lint_json must produce valid JSON");

    let arr = value.as_array().expect("top-level JSON value must be an array");
    assert!(!arr.is_empty(), "JSON array must not be empty");

    for item in arr {
        let obj = item.as_object().expect("each item must be a JSON object");
        assert!(
            obj.contains_key("path"),
            "each result object must have 'path'; got: {obj:?}"
        );
        assert!(
            obj.contains_key("diagnostics"),
            "each result object must have 'diagnostics'; got: {obj:?}"
        );

        let diags = obj["diagnostics"]
            .as_array()
            .expect("'diagnostics' must be a JSON array");
        for diag in diags {
            let d = diag.as_object().expect("each diagnostic must be a JSON object");

            assert!(d.contains_key("engine"), "diagnostic must have 'engine'; got: {d:?}");
            assert!(
                d.contains_key("severity"),
                "diagnostic must have 'severity'; got: {d:?}"
            );
            assert!(d.contains_key("title"), "diagnostic must have 'title'; got: {d:?}");

            assert!(
                !d["engine"].as_str().unwrap_or("").is_empty(),
                "'engine' must be a non-empty string; got: {d:?}"
            );
            assert!(
                !d["title"].as_str().unwrap_or("").is_empty(),
                "'title' must be a non-empty string; got: {d:?}"
            );
        }
    }

    let taplo_diag = arr
        .iter()
        .flat_map(|item| item["diagnostics"].as_array().into_iter().flatten())
        .find(|d| d["engine"].as_str() == Some("taplo"))
        .expect("expected a taplo diagnostic in the JSON output");

    let d = taplo_diag.as_object().expect("taplo diagnostic must be a JSON object");

    assert!(
        d.contains_key("code"),
        "taplo duplicate-key diagnostic must have 'code'; got: {d:?}"
    );
    assert!(
        d.contains_key("span"),
        "taplo duplicate-key diagnostic must have 'span'; got: {d:?}"
    );
    let span = d["span"].as_object().expect("'span' must be a JSON object");
    assert!(
        span.contains_key("start_line"),
        "'span' must have 'start_line'; got: {span:?}"
    );
    assert!(
        span.contains_key("start_col"),
        "'span' must have 'start_col'; got: {span:?}"
    );

    assert!(
        !d.contains_key("description"),
        "'description' must be absent (not serialised) when None; got: {d:?}"
    );
    assert!(!d.contains_key("url"), "'url' must be absent when None; got: {d:?}");

    assert!(!d.contains_key("fix"), "'fix' must be absent when empty; got: {d:?}");

    assert!(
        !d.contains_key("metadata"),
        "'metadata' must be absent when empty; got: {d:?}"
    );
}

/// The template skip is decided once per file, by `Engine::skip_reason`, and that
/// one answer now stands in for running the format chain. So the answer itself is
/// what needs pinning: a live template stays out of the formatter, and a file that
/// merely *documents* template syntax stays in it.
#[test]
fn fmt_skips_live_templates_but_still_formats_documented_template_syntax() {
    common::register();
    let dir = tempfile::tempdir().expect("tempdir");

    // Markdown whose template action is live prose: never reformatted.
    write(dir.path(), "chart.md", "# {{ .Chart.Name }}\n\nDeployed.   \n");
    // Markdown documenting the same syntax inside code: still formatted (the
    // trailing whitespace below is real drift the formatter must report).
    write(
        dir.path(),
        "docs.md",
        "# Docs\n\nUse `{{ .Values.image }}` in a chart:   \n\n```yaml\nimage: {{ .Values.image }}\n```\n",
    );
    // Helm YAML: not valid YAML, so the backend declines it.
    write(
        dir.path(),
        "deployment.yaml",
        "{{- if .Values.enabled }}\nkind:   Deployment\n{{- end }}\n",
    );

    let opts = RunOptions {
        no_cache: true,
        explicit_config: true,
        ..RunOptions::default()
    };
    let results =
        poly_core::format(&[dir.path().to_path_buf()], &Config::default(), &opts, false, false).expect("format run");
    let result = |name: &str| {
        results
            .iter()
            .find(|r| r.path.file_name().is_some_and(|f| f == name))
            .unwrap_or_else(|| panic!("no result for {name}, got {:?}", results))
    };

    for templated in ["chart.md", "deployment.yaml"] {
        let result = result(templated);
        assert_eq!(
            result.skipped.as_deref(),
            Some("Go/Helm template syntax"),
            "{templated} must be reported as skipped"
        );
        assert!(!result.changed, "{templated} must not be reformatted");
        assert!(result.formatted.is_none(), "{templated} must produce no output");
    }

    let documented = result("docs.md");
    assert_eq!(
        documented.skipped, None,
        "documented template syntax must stay in the format gate"
    );
    assert!(
        documented.changed,
        "docs.md carries trailing whitespace and must be reported as drifting"
    );
}
