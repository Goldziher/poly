//! Switching a web engine off by config: the run names the config table that did it.

use poly_core::{Config, LintRun, RunOptions};

mod common;

fn options() -> RunOptions {
    RunOptions {
        no_cache: true,
        jobs: Some(1),
        explicit_config: true,
        ..RunOptions::default()
    }
}

fn config(lint: &str) -> Config {
    Config {
        lint: toml::from_str(lint).expect("valid lint config"),
        ..Config::default()
    }
}

fn lint(dir: &std::path::Path, config: &Config) -> LintRun {
    common::register();
    poly_core::lint_run(&[dir.to_path_buf()], config, &options(), false, false).expect("lint run")
}

/// Withdrawing a language's only lint backend by config names **the config**,
/// not poly. Before this, a user who switched off their own linter was told
/// `no lint rules for TOML` — which reads as a poly limitation and sends them
/// looking for a backend that is right there, switched off in their own file.
///
/// TOML is the clean case: `taplo` is its only source of lint rules, so
/// disabling it genuinely removes the language's coverage rather than one check
/// among several.
#[test]
fn disabling_a_language_s_only_linter_names_the_config_table() {
    let dir = tempfile::tempdir().expect("tmp");
    std::fs::write(dir.path().join("pyproject.toml"), "[tool.x]\na = 1\n").expect("write");
    let run = lint(dir.path(), &config("[toml.taplo]\nenabled = false\n"));
    let reason = run
        .skipped
        .iter()
        .find(|s| s.path.file_name().is_some_and(|f| f == "pyproject.toml"))
        .map(|s| s.reason.clone())
        .expect("the file is reported as skipped");
    assert!(
        reason.contains("[lint.toml.taplo]"),
        "the reason must quote the table that did it, got {reason:?}"
    );
    assert!(
        poly_core::is_withdrawal_reason(&reason),
        "a caller instruction must not be charged to the skip budget: {reason:?}"
    );
}
