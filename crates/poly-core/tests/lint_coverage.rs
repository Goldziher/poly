//! What `N file(s) linted` is allowed to count.
//!
//! A `.kt` file routes to the cross-cutting backends (spell-check, ast-grep,
//! comment removal) and to nothing that holds a Kotlin rule, yet the run counted
//! it exactly like the `.py` file ruff had just examined: `poly lint .` over a
//! Kotlin/Swift/Zig repository printed `No issues found. (3 file(s) linted)` and
//! exited 0 with no rule in the process knowing any of those three languages.
//!
//! These tests pin the two halves that make the difference visible — the count,
//! and the reason attached to what the count leaves out — because a run that
//! merely exits 0 passes just as well against the broken behaviour.

use std::path::{Path, PathBuf};

use poly_core::report::{Verbosity, render_lint_pretty_run};
use poly_core::{Config, LintRun, RunOptions, SkippedFile};

/// Options for a self-contained run: no cache (so a rerun cannot answer from a
/// previous one), single-threaded, and no config discovery beyond the fixture.
fn options() -> RunOptions {
    RunOptions {
        no_cache: true,
        jobs: Some(1),
        explicit_config: true,
        ..RunOptions::default()
    }
}

fn write(dir: &Path, name: &str, content: &str) {
    std::fs::write(dir.join(name), content).expect("write fixture");
}

fn lint(dir: &Path) -> LintRun {
    poly_core::lint_run(&[dir.to_path_buf()], &Config::default(), &options(), false, false).expect("lint run")
}

/// The skipped set as `(file name, reason)` pairs, which is what a consumer
/// actually asserts on.
fn skips(run: &LintRun) -> Vec<(String, String)> {
    run.skipped
        .iter()
        .map(|SkippedFile { path, reason }| {
            (
                path.file_name().expect("named file").to_string_lossy().into_owned(),
                reason.clone(),
            )
        })
        .collect()
}

/// The reported defect, at the counting layer: one Kotlin file, nothing in poly
/// holds a Kotlin rule, so nothing was linted — and the run says so instead of
/// counting the file.
#[test]
fn language_with_no_lint_rules_is_not_counted_as_linted() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "a.kt", "fun main() {}\n");

    let run = lint(dir.path());

    assert_eq!(run.checked, 0, "no rule in this run knows Kotlin");
    assert_eq!(
        skips(&run),
        vec![("a.kt".to_owned(), "no lint rules for Kotlin".to_owned())]
    );
}

/// The mixed case is the dangerous one: with a linted file beside it, a wrong
/// count still looks plausible. Python is linted, Kotlin is not, and the summary
/// must split them one and one rather than reporting two.
#[test]
fn mixed_corpus_counts_only_the_files_a_rule_examined() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "a.kt", "fun main() {}\n");
    write(dir.path(), "d.py", "x = 1\n");

    let run = lint(dir.path());

    assert_eq!(run.checked, 1, "only the Python file was linted");
    assert_eq!(
        skips(&run),
        vec![("a.kt".to_owned(), "no lint rules for Kotlin".to_owned())]
    );

    let (text, total) = render_lint_pretty_run(&run, Verbosity::default());
    assert_eq!(total, 0);
    assert_eq!(
        text.lines().next(),
        Some("No issues found. (1 file(s) linted, 1 skipped (no lint rules for Kotlin))"),
        "got:\n{text}"
    );
}

/// The reason names the language, so the reader knows *what* poly cannot lint.
/// Distinct wording from `no matching engine for this file type`, which would be
/// false here — an engine matched, poly simply has no Swift rules, and the fix
/// for that is a Swift linter rather than a rename or an exclude.
#[test]
fn the_reason_names_the_language_and_is_not_the_no_engine_reason() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "app.swift", "let x = 1\n");

    let run = lint(dir.path());

    assert_eq!(
        skips(&run),
        vec![("app.swift".to_owned(), "no lint rules for Swift".to_owned())]
    );
    assert!(
        !run.skipped.iter().any(|s| s.reason == poly_core::NO_ENGINE_SKIP),
        "a routed language is not an unmatched file type"
    );
}

/// A language poly does lint keeps its place in the count and adds no
/// narration: the common path must not gain a skip line.
#[test]
fn a_language_with_rules_is_still_counted_and_silent() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "d.py", "x = 1\n");
    write(dir.path(), "c.toml", "a = 1\n");

    let run = lint(dir.path());

    assert_eq!(run.checked, 2);
    assert_eq!(skips(&run), Vec::new());
    let (text, _) = render_lint_pretty_run(&run, Verbosity::default());
    assert_eq!(text, "No issues found. (2 file(s) linted)\n");
}

/// A file the walk could not identify at all is not a skip — see
/// `DiscoveryReport::unrecognized_files` for why itemising a walk's unreadable
/// files would fire `--deny-skips` in every repository — but it is counted and
/// named, so it cannot be mistaken for a file that was checked.
#[test]
fn unknown_extension_is_counted_as_unrecognized_not_as_a_skip() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "c.xyz", "zzz\n");
    write(dir.path(), "d.py", "x = 1\n");

    let run = lint(dir.path());

    assert_eq!(run.checked, 1, "only the Python file was linted");
    assert_eq!(skips(&run), Vec::new(), "a walked file poly cannot read is not a skip");
    assert_eq!(run.discovery.unrecognized_files, 1);
    assert_eq!(
        run.discovery.unrecognized_samples,
        vec![dir.path().join("c.xyz")],
        "the number alone leaves the reader guessing which file it was"
    );

    let (text, _) = render_lint_pretty_run(&run, Verbosity::default());
    assert_eq!(
        text.lines().next(),
        Some("No issues found. (1 file(s) linted, 1 file(s) of unrecognized type not checked)"),
        "got:\n{text}"
    );
    assert!(
        text.contains("were not identified as any language and no engine saw them (e.g. "),
        "got:\n{text}"
    );
}

/// A path *named* by the caller that no engine covers stays a skip: naming a
/// path is a request to check it. The two accountings must not double-report it.
#[test]
fn an_explicitly_named_unknown_file_is_a_skip_and_is_not_double_counted() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "c.xyz", "zzz\n");

    let run = poly_core::lint_run(
        &[dir.path().join("c.xyz")],
        &Config::default(),
        &options(),
        false,
        false,
    )
    .expect("lint run");

    assert_eq!(run.checked, 0);
    assert_eq!(
        skips(&run),
        vec![("c.xyz".to_owned(), poly_core::NO_ENGINE_SKIP.to_owned())]
    );
    assert_eq!(
        run.discovery.unrecognized_files, 0,
        "already reported as a skip; counting it again would name it twice under two headings"
    );
}

/// The cross-cutting backends still run over an uncovered file, so its findings
/// are still reported — being uncovered by a *language* rule is not a reason to
/// stop spell-checking it, and the skip does not retract the finding.
#[test]
fn cross_cutting_findings_survive_on_a_file_with_no_language_rules() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "a.kt", "// teh quick brown fox\nfun main() {}\n");

    let run = lint(dir.path());

    assert_eq!(run.checked, 0);
    assert_eq!(
        skips(&run),
        vec![("a.kt".to_owned(), "no lint rules for Kotlin".to_owned())]
    );
    let typos: Vec<&str> = run
        .results
        .iter()
        .flat_map(|result| &result.diagnostics)
        .map(|diagnostic| diagnostic.engine.as_str())
        .collect();
    assert_eq!(typos, vec!["typos"], "the spell-check finding must survive the skip");
    let paths: Vec<PathBuf> = run.results.iter().map(|result| result.path.clone()).collect();
    assert_eq!(paths, vec![dir.path().join("a.kt")]);
    assert_eq!(
        run.results[0].skipped.as_deref(),
        Some("no lint rules for Kotlin"),
        "the per-file record carries the reason alongside the finding"
    );
}
