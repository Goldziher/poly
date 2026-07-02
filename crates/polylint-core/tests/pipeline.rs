//! End-to-end pipeline tests for the core: discovery → routing → run → cache.

use std::fs;

use polylint_core::report::report_lint_json;
use polylint_core::{Config, RunOptions};

fn write(dir: &std::path::Path, name: &str, content: &str) -> std::path::PathBuf {
    let p = dir.join(name);
    fs::write(&p, content).unwrap();
    p
}

#[test]
fn lint_flags_trailing_whitespace() {
    // Use a Go file: its NativeToolEngine slot delegates `lint` to the
    // tree-sitter generic tier, which emits the catch-all trailing-whitespace
    // diagnostic (gofmt is format-only). (TOML→taplo and YAML→yaml are native
    // backends that do not.) The lint is purely textual, so no grammar download
    // happens here.
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.go", "package main   \nfunc main() {}\n");
    let cfg = Config::default();
    let opts = RunOptions {
        no_cache: true,
        jobs: Some(1),
        exclude: Vec::new(),
    };
    let results = polylint_core::lint(&[dir.path().to_path_buf()], &cfg, &opts, false, false).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].diagnostics.len(), 1);
    assert_eq!(results[0].diagnostics[0].code.as_deref(), Some("trailing-whitespace"));
}

#[test]
fn format_check_does_not_write_but_reports_change() {
    let dir = tempfile::tempdir().unwrap();
    let messy = "x = 1   \n\n\n";
    let path = write(dir.path(), "a.toml", messy);
    let cfg = Config::default();
    let opts = RunOptions {
        no_cache: true,
        jobs: Some(1),
        exclude: Vec::new(),
    };

    let results = polylint_core::format(&[dir.path().to_path_buf()], &cfg, &opts, false, false).unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].changed, "check mode should detect a change");
    // File must be untouched in check mode.
    assert_eq!(fs::read_to_string(&path).unwrap(), messy);
}

#[test]
fn format_write_is_idempotent() {
    // Use a YAML file to test whitespace-engine idempotency (trailing ws +
    // blank lines normalized to one trailing newline). TOML now routes to the
    // taplo native backend which has different blank-line semantics.
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "a.yaml", "key: value   \n\n\n");
    let cfg = Config::default();
    let opts = RunOptions {
        no_cache: true,
        jobs: Some(1),
        exclude: Vec::new(),
    };

    let first = polylint_core::format(&[dir.path().to_path_buf()], &cfg, &opts, true, false).unwrap();
    assert!(first[0].changed);
    let after = fs::read_to_string(&path).unwrap();
    assert_eq!(after, "key: value\n", "trailing ws + blank lines normalized");

    // Second pass: nothing left to change.
    let second = polylint_core::format(&[dir.path().to_path_buf()], &cfg, &opts, true, false).unwrap();
    assert!(!second[0].changed, "formatting must be idempotent");
}

#[test]
fn lint_fix_applies_autofixes_and_dry_run_does_not() {
    // The misspellings live in an excluded fixture so the `typos` pre-commit
    // hook cannot "correct" this test's source out from under it.
    let bad = include_str!("fixtures/typos/known_bad.txt");
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "notes.md", bad);
    let cfg = Config::default();
    let opts = RunOptions {
        no_cache: true,
        jobs: Some(1),
        exclude: Vec::new(),
    };

    // Dry run (fix = false) must not touch the file on disk.
    polylint_core::lint(&[dir.path().to_path_buf()], &cfg, &opts, false, false).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), bad, "dry run must not modify files");

    // fix = true applies the single-correction typo autofixes in place.
    polylint_core::lint(&[dir.path().to_path_buf()], &cfg, &opts, true, false).unwrap();
    let fixed = fs::read_to_string(&path).unwrap();
    assert_ne!(fixed, bad, "fix must rewrite the file");
    // Assert the exact corrected output. It uses only correctly-spelled words,
    // so the `typos` pre-commit hook cannot rewrite this source and silently
    // break the assertion (the four misspellings stay in the excluded fixture).
    assert_eq!(
        fixed, "The language of the receive function.\nThis is the occurrence of a typo.\n",
        "all four single-correction typos should be autofixed in place",
    );
}

#[test]
fn cache_round_trips() {
    use poly_cache::{Namespace, ResultCache};
    let dir = tempfile::tempdir().unwrap();
    let cache = ResultCache::open(dir.path().join("cache"), true).unwrap();
    let opts = toml::Table::new();
    let digest = ResultCache::single_file_digest("some content");
    let key = ResultCache::key(Namespace::Fmt, "test", "1", &opts, &digest);
    cache.put(Namespace::Fmt, &key, b"formatted").unwrap();
    assert_eq!(cache.get(Namespace::Fmt, &key).as_deref(), Some(&b"formatted"[..]));
    // A different version yields a different key (invalidation).
    let key2 = ResultCache::key(Namespace::Fmt, "test", "2", &opts, &digest);
    assert_ne!(key, key2);
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
    let dir = tempfile::tempdir().unwrap();
    // Duplicate key: taplo always produces a `duplicate-key` diagnostic with
    // code + span but no description, url, fix, or metadata.
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
    };

    let results = polylint_core::lint(&[dir.path().to_path_buf()], &cfg, &opts, false, false).unwrap();

    assert!(
        !results.is_empty(),
        "expected diagnostics from the duplicate-key TOML fixture"
    );

    let json = report_lint_json(&results);
    let value: serde_json::Value = serde_json::from_str(&json).expect("report_lint_json must produce valid JSON");

    // --- Envelope: top-level array of { path, diagnostics } ---
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

            // Required keys must be present.
            assert!(d.contains_key("engine"), "diagnostic must have 'engine'; got: {d:?}");
            assert!(
                d.contains_key("severity"),
                "diagnostic must have 'severity'; got: {d:?}"
            );
            assert!(d.contains_key("title"), "diagnostic must have 'title'; got: {d:?}");

            // Required fields must be non-empty strings.
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

    // --- taplo-specific: structured fields present, optional fields absent ---
    // Find the taplo duplicate-key diagnostic (first TOML diagnostic in the results).
    let taplo_diag = arr
        .iter()
        .flat_map(|item| item["diagnostics"].as_array().into_iter().flatten())
        .find(|d| d["engine"].as_str() == Some("taplo"))
        .expect("expected a taplo diagnostic in the JSON output");

    let d = taplo_diag.as_object().expect("taplo diagnostic must be a JSON object");

    // taplo duplicate-key always sets code + span.
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

    // taplo does not set description, url — they must be ABSENT (not null).
    assert!(
        !d.contains_key("description"),
        "'description' must be absent (not serialised) when None; got: {d:?}"
    );
    assert!(!d.contains_key("url"), "'url' must be absent when None; got: {d:?}");

    // Empty Vec<Edit> must not produce a 'fix' key.
    assert!(!d.contains_key("fix"), "'fix' must be absent when empty; got: {d:?}");

    // Empty BTreeMap metadata must not produce a 'metadata' key.
    assert!(
        !d.contains_key("metadata"),
        "'metadata' must be absent when empty; got: {d:?}"
    );
}
