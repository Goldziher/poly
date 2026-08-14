//! End-to-end pipeline tests for the core: discovery → routing → run → cache.

use std::fs;

use poly_core::report::report_lint_json;
use poly_core::{Config, RunOptions};

fn write(dir: &std::path::Path, name: &str, content: &str) -> std::path::PathBuf {
    let p = dir.join(name);
    fs::write(&p, content).unwrap();
    p
}

#[test]
fn lint_does_not_flag_trailing_whitespace() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.go", "package main   \nfunc main() {}\n");
    let cfg = Config::default();
    let opts = RunOptions {
        no_cache: true,
        jobs: Some(1),
        exclude: Vec::new(),
        force_exclude: false,
        fix_generated: false,
        explicit_config: true,
        config_resolver: None,
        externally_linted_languages: Vec::new(),
    };
    let results = poly_core::lint(&[dir.path().to_path_buf()], &cfg, &opts, false, false).unwrap();
    assert!(
        results.iter().all(|r| r.diagnostics.is_empty()),
        "trailing whitespace must not surface as a lint diagnostic, got {:?}",
        results.iter().flat_map(|r| &r.diagnostics).collect::<Vec<_>>()
    );
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
        force_exclude: false,
        fix_generated: false,
        explicit_config: true,
        config_resolver: None,
        externally_linted_languages: Vec::new(),
    };

    let results = poly_core::format(&[dir.path().to_path_buf()], &cfg, &opts, false, false).unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].changed, "check mode should detect a change");
    assert_eq!(fs::read_to_string(&path).unwrap(), messy);
}

#[test]
fn format_write_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "a.yaml", "key: value   \n\n\n");
    let cfg = Config::default();
    let opts = RunOptions {
        no_cache: true,
        jobs: Some(1),
        exclude: Vec::new(),
        force_exclude: false,
        fix_generated: false,
        explicit_config: true,
        config_resolver: None,
        externally_linted_languages: Vec::new(),
    };

    let first = poly_core::format(&[dir.path().to_path_buf()], &cfg, &opts, true, false).unwrap();
    assert!(first[0].changed);
    let after = fs::read_to_string(&path).unwrap();
    assert_eq!(after, "key: value\n", "trailing ws + blank lines normalized");

    let second = poly_core::format(&[dir.path().to_path_buf()], &cfg, &opts, true, false).unwrap();
    assert!(!second[0].changed, "formatting must be idempotent");
}

#[cfg(unix)]
#[test]
fn format_write_preserves_the_executable_bit() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "script.yaml", "key: value   \n\n\n");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    let cfg = Config::default();
    let opts = RunOptions {
        no_cache: true,
        jobs: Some(1),
        exclude: Vec::new(),
        force_exclude: false,
        fix_generated: false,
        explicit_config: true,
        config_resolver: None,
        externally_linted_languages: Vec::new(),
    };

    let results = poly_core::format(&[dir.path().to_path_buf()], &cfg, &opts, true, false).unwrap();
    assert!(
        results[0].changed,
        "the fixture must actually be rewritten for this to prove anything"
    );
    let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o755,
        "the atomic rewrite must carry the original mode across the rename"
    );
}

#[test]
fn lint_fix_applies_autofixes_and_dry_run_does_not() {
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
        explicit_config: true,
        config_resolver: None,
        externally_linted_languages: Vec::new(),
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
        explicit_config: true,
        config_resolver: None,
        externally_linted_languages: Vec::new(),
    };

    let results = poly_core::lint(&[dir.path().to_path_buf()], &cfg, &opts, false, false).unwrap();

    assert!(
        !results.is_empty(),
        "expected diagnostics from the duplicate-key TOML fixture"
    );

    let json = report_lint_json(&results);
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

/// A directory walk prunes excluded paths, but a file named on the command line
/// was always kept. That is right when a human names a file and wrong in a hook,
/// which is *always* handed explicit staged paths — so a repo excluding
/// `**/*.tf` found the exclude silently inert in its pre-commit hook, and poly
/// reformatted Terraform that `terraform fmt` then rejected.
#[test]
fn force_exclude_applies_the_exclude_set_to_explicitly_named_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::write(root.join("poly.toml"), "[discovery]\nexclude = [\"**/*.tf\"]\n").expect("write config");
    std::fs::create_dir_all(root.join("terraform")).expect("mkdir");
    let excluded = root.join("terraform/main.tf");
    std::fs::write(&excluded, "variable   \"a\"   {}\n").expect("write tf");
    let kept = root.join("app.py");
    std::fs::write(&kept, "x   =    1\n").expect("write py");

    let config = poly_core::Config {
        exclude: vec!["**/*.tf".to_string()],
        ..poly_core::Config::default()
    };
    let run = |paths: &[std::path::PathBuf], force: bool| {
        let opts = RunOptions {
            no_cache: true,
            force_exclude: force,
            ..RunOptions::default()
        };
        poly_core::format(paths, &config, &opts, false, false).expect("format")
    };

    // Naming the file explicitly checks it by default — that behaviour stands.
    assert_eq!(run(std::slice::from_ref(&excluded), false).len(), 1);

    // Under force_exclude the same explicit path is excluded...
    assert!(run(std::slice::from_ref(&excluded), true).is_empty());

    // ...while a path the exclude set does not match is still checked, so this
    // cannot be mistaken for "force_exclude drops everything".
    assert_eq!(run(std::slice::from_ref(&kept), true).len(), 1);
}

/// `--fix` on a generated file is churn the next generation run reverts, and it
/// can silence the diagnostic that was the only evidence of a generator bug.
///
/// The motivating case: ruff's `F841` fired on an unused binding in a generated
/// test, and that binding was the sole signal that 39 generated tests across 8
/// files called an API and asserted nothing. `--fix` rewrote it to `_` and
/// turned a correct diagnostic about a real upstream defect into a clean pass.
#[test]
fn fix_is_withheld_on_generated_files_but_diagnostics_are_still_reported() {
    let dir = tempfile::tempdir().expect("tempdir");
    let generated = dir.path().join("generated.py");
    // `import asyncio` is unused, which ruff reports with an autofix — the
    // rewrite this test is about. `result` additionally reproduces the reported
    // F841, which carries no autofix by default.
    let body = "import asyncio\n\n\nasync def test_extract(inputs):\n    result = await extract_batch(inputs, None)\n";
    std::fs::write(
        &generated,
        format!("# This file is auto-generated by alef — DO NOT EDIT.\n{body}"),
    )
    .expect("write generated");
    let handwritten = dir.path().join("handwritten.py");
    std::fs::write(&handwritten, body).expect("write handwritten");

    let fix = |path: &std::path::Path, fix_generated: bool| {
        let before = std::fs::read_to_string(path).expect("read");
        let opts = RunOptions {
            no_cache: true,
            fix_generated,
            ..RunOptions::default()
        };
        let results = poly_core::lint(
            std::slice::from_ref(&path.to_path_buf()),
            &Config::default(),
            &opts,
            true,
            false,
        )
        .expect("lint");
        let after = std::fs::read_to_string(path).expect("read");
        (results, before != after)
    };

    // The generated file is reported on, and left on disk exactly as it was.
    let (results, rewritten) = fix(&generated, false);
    assert!(!rewritten, "generated file must not be rewritten by --fix");
    assert!(!results.is_empty(), "diagnostics must still be reported");
    assert!(
        results.iter().any(|r| r.fix_withheld_generated),
        "the withheld fix must be visible to the reporter, not silent"
    );

    // A handwritten file with the same defect is still fixed, so this cannot be
    // mistaken for "--fix stopped working".
    let (_, rewritten) = fix(&handwritten, false);
    assert!(rewritten, "handwritten file must still be fixed");

    // And the escape hatch opts back in.
    let (_, rewritten) = fix(&generated, true);
    assert!(rewritten, "--fix-generated must apply fixes to generated files");
}

/// `fmt` skipping a file removes it from the format gate entirely, so it must
/// require a *content hash* — not a bare "DO NOT EDIT" banner.
///
/// Generalising this caused real harm: a generator stamping a hand-written file
/// with a banner silently dropped that file out of lint and format enforcement,
/// and a consumer's most user-facing code left the gate with nothing reporting
/// it. Reformatting a banner-only file is harmless; not checking it is not.
#[test]
fn fmt_skips_hash_stamped_files_but_not_banner_only_ones() {
    let dir = tempfile::tempdir().expect("tempdir");
    let banner = dir.path().join("banner_only.py");
    std::fs::write(&banner, "# This file is managed by alef — DO NOT EDIT.\nx   =    1\n").expect("write");
    let hashed = dir.path().join("hashed.py");
    std::fs::write(
        &hashed,
        "# Auto-generated — DO NOT EDIT.\n# alef:hash: deadbeef\ny   =    2\n",
    )
    .expect("write");

    let check = |path: &std::path::Path| {
        let opts = RunOptions {
            no_cache: true,
            ..RunOptions::default()
        };
        poly_core::format(
            std::slice::from_ref(&path.to_path_buf()),
            &Config::default(),
            &opts,
            false,
            false,
        )
        .expect("format")
    };

    // Banner only: still checked, and its drift is still reported.
    let results = check(&banner);
    assert!(
        results.iter().any(|r| r.changed),
        "a banner-only file must stay in the format gate"
    );
    assert!(results.iter().all(|r| r.skipped.is_none()));

    // Hash-stamped: skipped, because reformatting would invalidate the hash.
    let results = check(&hashed);
    assert!(results.iter().all(|r| !r.changed));
    assert!(
        results.iter().any(|r| r.skipped.is_some()),
        "a hash-stamped file must be skipped and reported as skipped"
    );
}

#[test]
fn fmt_checks_explicit_rust_hash_module_import() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = write(dir.path(), "hash_import.rs", "use a::hash::b;\n");
    let options = RunOptions {
        no_cache: true,
        ..RunOptions::default()
    };

    let results = poly_core::format(
        std::slice::from_ref(&source),
        &Config::default(),
        &options,
        false,
        false,
    )
    .expect("format explicit Rust source");

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].path, source);
    assert_eq!(results[0].skipped, None);
    assert_eq!(results[0].error, None);
}

/// The template skip is decided once per file, by `Engine::skip_reason`, and that
/// one answer now stands in for running the format chain. So the answer itself is
/// what needs pinning: a live template stays out of the formatter, and a file that
/// merely *documents* template syntax stays in it.
#[test]
fn fmt_skips_live_templates_but_still_formats_documented_template_syntax() {
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

/// A path named on the command line is a request to check that path, so one that
/// no engine covers has to be named back. The mixed invocation is the case that
/// matters: reconciling one explicit argument against a whole discovered corpus
/// must give the same answer as reconciling many.
#[test]
fn lint_names_unmatched_explicit_paths_alongside_a_directory() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "app.py", "x = 1\n");
    // Enough arguments to cross the threshold where the reconciliation switches
    // from scanning the discovered set to indexing it — both paths must agree.
    let unmatched: Vec<std::path::PathBuf> = (0..9)
        .map(|i| {
            write(
                dir.path(),
                &format!("notes{i}.unknownext"),
                "not a language poly knows\n",
            )
        })
        .collect();

    let opts = RunOptions {
        no_cache: true,
        explicit_config: true,
        ..RunOptions::default()
    };
    let skipped_for = |paths: &[std::path::PathBuf]| -> Vec<std::path::PathBuf> {
        let mut paths: Vec<std::path::PathBuf> = poly_core::lint_run(paths, &Config::default(), &opts, false, false)
            .expect("lint run")
            .skipped
            .into_iter()
            .filter(|s| s.reason == poly_core::runner::NO_ENGINE_SKIP)
            .map(|s| s.path)
            .collect();
        paths.sort();
        paths
    };

    // A directory walk alone narrates nothing: what a walk does not match is not
    // a skip.
    assert_eq!(
        skipped_for(&[dir.path().to_path_buf()]),
        Vec::<std::path::PathBuf>::new()
    );

    // One explicit path plus a directory — the scanned branch.
    assert_eq!(
        skipped_for(&[unmatched[0].clone(), dir.path().to_path_buf()]),
        vec![unmatched[0].clone()],
        "a mixed invocation must still name the unmatched explicit path"
    );

    // Nine explicit paths plus a directory — the indexed branch, same answer.
    let mut mixed = unmatched.clone();
    mixed.push(dir.path().to_path_buf());
    let mut expected = unmatched.clone();
    expected.sort();
    assert_eq!(skipped_for(&mixed), expected);
}
