//! `--only` / `--skip`: narrowing a run to named engines.
//!
//! The point of the flag is verification — proving *which* backend produced (or
//! preserved) something — so the tests assert on the engines that actually ran,
//! read from the per-file debug record, rather than inferring it from whichever
//! diagnostics happened to survive.
//!
//! The second half is the accounting. Filtering empties a plan, and an empty
//! plan already had a meaning: "no matching engine for this file type". Saying
//! that about a `.rs` file because the caller asked for ruff is false, and it
//! fails a `--deny-skips` run for a reason the caller created on purpose. So a
//! narrowed run reports its own reason, and that reason is not charged to the
//! skip budget.

use std::path::{Path, PathBuf};

use poly_core::{Config, FILTERED_SKIP, LintRun, NO_ENGINE_SKIP, RunOptions, SkippedFile};

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

/// A Python file ruff has something to say about, plus a Rust file it does not
/// handle at all — the two halves the selection has to keep straight.
fn fixture(dir: &Path) {
    write(dir, "app.py", "import os\n");
    write(dir, "main.rs", "fn main() {}\n");
}

fn lint_with(dir: &Path, opts: RunOptions) -> LintRun {
    poly_core::lint_run(&[dir.to_path_buf()], &Config::default(), &opts, false, true).expect("lint run")
}

/// Engines that ran on `name`, sorted, from the `--debug` record.
fn engines_for_file(run: &LintRun, name: &str) -> Vec<String> {
    let mut engines: Vec<String> = run
        .results
        .iter()
        .filter(|result| result.path.file_name().is_some_and(|file| file == name))
        .filter_map(|result| result.debug.as_ref())
        .flat_map(|debug| debug.engines.iter().map(|entry| entry.engine.clone()))
        .collect();
    engines.sort();
    engines
}

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

fn reason_for(run: &LintRun, name: &str) -> Option<String> {
    skips(run)
        .into_iter()
        .find(|(file, _)| file == name)
        .map(|(_, reason)| reason)
}

#[test]
fn only_runs_the_named_engine_and_nothing_else() {
    let dir = tempfile::tempdir().expect("tmp");
    fixture(dir.path());
    let run = lint_with(
        dir.path(),
        RunOptions {
            only: vec!["ruff".to_string()],
            ..options()
        },
    );
    assert_eq!(engines_for_file(&run, "app.py"), vec!["ruff".to_string()]);
}

#[test]
fn an_unnarrowed_run_still_evaluates_every_routed_engine() {
    let dir = tempfile::tempdir().expect("tmp");
    fixture(dir.path());
    let run = lint_with(dir.path(), options());
    let engines = engines_for_file(&run, "app.py");
    assert!(engines.contains(&"ruff".to_string()), "{engines:?}");
    assert!(engines.contains(&"typos".to_string()), "{engines:?}");
}

#[test]
fn skip_removes_only_the_named_engine() {
    let dir = tempfile::tempdir().expect("tmp");
    fixture(dir.path());
    let run = lint_with(
        dir.path(),
        RunOptions {
            skip: vec!["typos".to_string()],
            ..options()
        },
    );
    let engines = engines_for_file(&run, "app.py");
    assert!(engines.contains(&"ruff".to_string()), "{engines:?}");
    assert!(!engines.contains(&"typos".to_string()), "{engines:?}");
}

/// The accounting half: a file the selection emptied is reported as unselected,
/// never as a file type poly does not handle.
#[test]
fn a_file_the_selection_emptied_reports_the_filtered_reason() {
    let dir = tempfile::tempdir().expect("tmp");
    fixture(dir.path());
    let run = lint_with(
        dir.path(),
        RunOptions {
            only: vec!["ruff".to_string()],
            ..options()
        },
    );
    let reason = reason_for(&run, "main.rs").expect("main.rs is skipped under --only ruff");
    assert_eq!(reason, FILTERED_SKIP, "got {reason:?}");
}

/// ...and the reason a genuinely unroutable path gets is unchanged, so the two
/// stay distinguishable.
#[test]
fn an_unroutable_named_path_still_reports_no_matching_engine() {
    let dir = tempfile::tempdir().expect("tmp");
    write(dir.path(), "notes.unknownext", "hello\n");
    let path: PathBuf = dir.path().join("notes.unknownext");
    let run = poly_core::lint_run(&[path], &Config::default(), &options(), false, false).expect("lint run");
    assert_eq!(reason_for(&run, "notes.unknownext").as_deref(), Some(NO_ENGINE_SKIP));
}

/// A name no engine answers to is a hard error, not a run that quietly checks
/// nothing: `--only ruffs` must not exit 0 having verified none of the tree.
#[test]
fn an_unrecognized_engine_name_fails_the_run() {
    let dir = tempfile::tempdir().expect("tmp");
    fixture(dir.path());
    let error = poly_core::lint_run(
        &[dir.path().to_path_buf()],
        &Config::default(),
        &RunOptions {
            only: vec!["ruffs".to_string()],
            ..options()
        },
        false,
        false,
    )
    .expect_err("an unknown engine name must fail the run");
    let message = error.to_string();
    assert!(message.contains("ruffs"), "the message must name the input: {message}");
}

/// Selection narrows; it never widens. `uncomment` is opt-in, so naming it
/// without enabling it selects nothing rather than switching it on.
#[test]
fn only_does_not_enable_an_opt_in_engine() {
    let dir = tempfile::tempdir().expect("tmp");
    fixture(dir.path());
    let run = lint_with(
        dir.path(),
        RunOptions {
            only: vec!["uncomment".to_string()],
            ..options()
        },
    );
    assert!(
        run.results.iter().all(|result| result.diagnostics.is_empty()),
        "an opt-in engine must stay off: {:?}",
        run.results
    );
}
