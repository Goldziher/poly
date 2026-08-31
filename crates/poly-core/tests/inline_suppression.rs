//! End-to-end inline suppression directives (ADR 0028), driven through the real
//! `lint` pipeline rather than the parser alone.
//!
//! Every language case is a *pair*: the same source without the directive must
//! report the diagnostic, and with the directive must not. Asserting only the
//! absence would pass just as happily against a backend that had stopped
//! reporting anything at all — which is the exact failure mode ADR 0028 calls
//! out (a suppression comment that is documented but read by no code).

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use poly_core::engine::Severity;
use poly_core::runner::SuppressionReason;
use poly_core::{Config, RunOptions, lint};

fn opts() -> RunOptions {
    RunOptions {
        force_exclude: false,
        fix_generated: false,
        generated: None,
        no_cache: true,
        jobs: Some(1),
        exclude: Vec::new(),
        explicit_config: true,
        config_resolver: None,
        externally_linted_languages: Vec::new(),
        only: Vec::new(),
        skip: Vec::new(),
    }
}

fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

/// Lint a single file with the given contents in a throwaway tree and return the
/// codes it reports. The `tempfile::TempDir` is returned alongside so callers
/// that need the file afterwards can keep it alive.
fn codes_for(name: &str, contents: &str) -> Vec<String> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    write(&path, contents);
    let results = lint(&[dir.path().to_path_buf()], &Config::default(), &opts(), false, false).unwrap();
    results
        .iter()
        .flat_map(|result| result.diagnostics.iter())
        .filter_map(|diagnostic| diagnostic.code.clone())
        .collect()
}

/// Assert that `code` is reported for `without` and gone for `with` — the pair
/// that proves the directive is what removed it.
fn assert_suppresses(name: &str, code: &str, without: &str, with: &str) {
    let before = codes_for(name, without);
    assert!(
        before.iter().any(|reported| reported == code),
        "{name}: expected {code} without a directive; got {before:?}"
    );
    let after = codes_for(name, with);
    assert!(
        !after.iter().any(|reported| reported == code),
        "{name}: {code} must be suppressed by the directive; got {after:?}"
    );
}

/// Python, `#` comment, trailing after the code it covers: ruff's F401.
#[test]
fn python_hash_directive_suppresses_ruff_f401() {
    assert_suppresses(
        "unused.py",
        "F401",
        "import os\n",
        "import os  # poly: allow[F401] re-exported for backwards compatibility\n",
    );
}

/// Python, `#` comment on its own line: covers the next non-blank line, so the
/// blank line between directive and target must not break the pairing.
#[test]
fn python_comment_only_directive_covers_the_next_non_blank_line() {
    assert_suppresses(
        "unused.py",
        "F401",
        "\n\nimport os\n",
        "# poly: allow[F401] re-exported for backwards compatibility\n\n\nimport os\n",
    );
}

/// TypeScript, `//` comment: oxlint's `no-debugger`.
#[test]
fn typescript_slash_directive_suppresses_oxlint_no_debugger() {
    assert_suppresses(
        "app.ts",
        "no-debugger",
        "export function stop(): void {\n    debugger;\n}\n",
        "export function stop(): void {\n    debugger; // poly: allow[no-debugger] deliberate breakpoint in the debug build\n}\n",
    );
}

/// JavaScript, `/* … */` block comment on its own line, covering the next line.
#[test]
fn javascript_block_comment_directive_suppresses_the_next_line() {
    assert_suppresses(
        "app.js",
        "no-debugger",
        "function stop() {\n    debugger;\n}\n",
        "function stop() {\n    /* poly: allow[no-debugger] deliberate breakpoint in the debug build */\n    debugger;\n}\n",
    );
}

/// TOML, `#` comment: the cross-cutting typos backend's `typo`.
#[test]
fn toml_hash_directive_suppresses_a_typo() {
    assert_suppresses(
        "config.toml",
        "typo",
        "key = \"teh\"\n",
        "key = \"teh\"  # poly: allow[typo] upstream spells the vendor key this way\n",
    );
}

/// YAML, `#` comment, same cross-cutting backend on a different language.
#[test]
fn yaml_hash_directive_suppresses_a_typo() {
    assert_suppresses(
        "values.yaml",
        "typo",
        "key: teh\n",
        "# poly: allow[typo] upstream spells the vendor key this way\nkey: teh\n",
    );
}

/// `allow-file` covers the whole file wherever it sits — here below both of the
/// findings it suppresses.
#[test]
fn allow_file_covers_findings_above_the_directive() {
    assert_suppresses(
        "app.js",
        "no-debugger",
        "debugger;\ndebugger;\n",
        "debugger;\ndebugger;\n// poly: allow-file[no-debugger] debug-only entrypoint, excluded from the bundle\n",
    );
}

/// A rule the directive does not name keeps firing: suppression is per-rule, not
/// a blanket line mute.
#[test]
fn an_unnamed_rule_on_the_same_line_still_fires() {
    let codes = codes_for(
        "app.js",
        "debugger; console.log(1); // poly: allow[no-debugger] deliberate breakpoint\n",
    );
    assert!(
        !codes.iter().any(|c| c == "no-debugger"),
        "the named rule is suppressed; got {codes:?}"
    );
    assert!(
        codes.iter().any(|c| c == "no-console"),
        "an unnamed rule must survive; got {codes:?}"
    );
}

/// The guard rail: a directive with no reason suppresses nothing and reports
/// `lazy-ignore` instead. Trading an `Error` for a `Warning` would be a CI
/// bypass, since `poly lint` only fails on `Error`.
#[test]
fn an_unjustified_directive_reports_lazy_ignore_and_does_not_suppress() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("unused.py");
    write(&path, "import os  # poly: allow[F401]\n");

    let results = lint(&[dir.path().to_path_buf()], &Config::default(), &opts(), false, false).unwrap();
    let diagnostics: Vec<_> = results.iter().flat_map(|r| r.diagnostics.iter()).collect();

    assert!(
        diagnostics.iter().any(|d| d.code.as_deref() == Some("F401")),
        "an unjustified directive must not suppress; got {diagnostics:?}"
    );
    let lazy: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code.as_deref() == Some("lazy-ignore"))
        .collect();
    assert_eq!(lazy.len(), 1, "exactly one lazy-ignore; got {diagnostics:?}");
    assert_eq!(lazy[0].engine, "poly");
    assert_eq!(lazy[0].severity, Severity::Warning);
    assert_eq!(lazy[0].span.unwrap().start_line, 1);
}

/// A `lazy-ignore` finding is still config-suppressible, which is why the runner
/// applies `[per-file-ignores]` after the inline pass rather than before it.
#[test]
fn per_file_ignores_can_silence_lazy_ignore() {
    let dir = tempfile::tempdir().unwrap();
    write(&dir.path().join("unused.py"), "import os  # poly: allow[F401]\n");

    let mut per_file_ignores = BTreeMap::new();
    per_file_ignores.insert("**".to_string(), vec!["lazy-ignore".to_string()]);
    let config = Config {
        per_file_ignores,
        ..Config::default()
    };

    let results = lint(&[dir.path().to_path_buf()], &config, &opts(), false, false).unwrap();
    let codes: Vec<_> = results
        .iter()
        .flat_map(|r| r.diagnostics.iter())
        .filter_map(|d| d.code.clone())
        .collect();
    assert!(
        !codes.iter().any(|c| c == "lazy-ignore"),
        "per-file-ignores must be able to silence lazy-ignore; got {codes:?}"
    );
    assert!(codes.iter().any(|c| c == "F401"), "the rule itself still fires");
}

/// A directive-shaped string literal is not a directive: the character before
/// the marker is a quote, which is deliberately not a comment opener.
#[test]
fn a_string_literal_does_not_suppress() {
    let codes = codes_for("app.js", "const s = \"poly: allow[no-debugger] nope\";\ndebugger;\n");
    assert!(
        codes.iter().any(|c| c == "no-debugger"),
        "a string literal must not suppress; got {codes:?}"
    );
}

/// `--fix` must not rewrite a suppressed finding, and the suppression must
/// survive the line shift a fix causes: removing line 1 moves the directive (and
/// its target) up, so the set has to be rebuilt from the rewritten content.
#[test]
fn fix_respects_a_directive_and_survives_the_line_shift_it_causes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("imports.py");
    let source = "import os\nimport sys  # poly: allow[F401] re-exported for the plugin loader\n";
    write(&path, source);

    lint(&[dir.path().to_path_buf()], &Config::default(), &opts(), true, false).unwrap();

    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "import sys  # poly: allow[F401] re-exported for the plugin loader\n",
        "the unsuppressed import is removed; the suppressed one survives the shift to line 1"
    );
}

/// An inline directive is a caller instruction, and by the rule the reporting
/// surface follows it must name itself rather than dropping a finding in
/// silence. The suppressed entry carries the file, the rule, and the mechanism.
#[test]
fn an_inline_directive_names_itself_in_the_suppressed_list() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("unused.py");
    write(
        &path,
        "import os  # poly: allow[F401] re-exported for the plugin loader\n",
    );

    let run = poly_core::lint_run(&[dir.path().to_path_buf()], &Config::default(), &opts(), false, false).unwrap();

    let entries: Vec<_> = run
        .suppressed
        .iter()
        .filter(|s| s.code.as_deref() == Some("F401"))
        .collect();
    assert_eq!(entries.len(), 1, "one suppressed finding, got {:?}", run.suppressed);
    assert_eq!(entries[0].path, path);
    assert_eq!(entries[0].reason, SuppressionReason::InlineSuppression);
}

/// `raw == reported + suppressed` for the inline mechanism: the same file
/// without its directive reports exactly what the directive-carrying run
/// reports plus what it recorded as suppressed.
#[test]
fn reported_plus_suppressed_reconstructs_the_findings_a_directive_hid() {
    let with = "import os  # poly: allow[F401] re-exported for the plugin loader\n";
    let without = "import os\n";

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("unused.py");
    write(&path, with);
    let run = poly_core::lint_run(&[dir.path().to_path_buf()], &Config::default(), &opts(), false, false).unwrap();

    let mut reconstructed: Vec<String> = run
        .results
        .iter()
        .flat_map(|r| r.diagnostics.iter())
        .filter_map(|d| d.code.clone())
        .collect();
    reconstructed.extend(run.suppressed.iter().filter_map(|s| s.code.clone()));
    reconstructed.sort();

    let mut raw = codes_for("unused.py", without);
    raw.sort();

    assert_eq!(reconstructed, raw, "the unfiltered set is reconstructible from one run");
}

/// A `--fix` run re-runs the suppression filters once per fix pass; the
/// suppressed list must describe the final state, not accumulate a copy per
/// pass.
#[test]
fn a_fix_run_does_not_double_count_a_suppression() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("imports.py");
    write(
        &path,
        "import os\nimport sys  # poly: allow[F401] re-exported for the plugin loader\n",
    );

    let run = poly_core::lint_run(&[dir.path().to_path_buf()], &Config::default(), &opts(), true, false).unwrap();

    let entries: Vec<_> = run
        .suppressed
        .iter()
        .filter(|s| s.code.as_deref() == Some("F401"))
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "exactly one suppression survives the fix passes, got {:?}",
        run.suppressed
    );
}
