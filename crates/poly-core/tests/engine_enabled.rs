//! Switching one engine off — and back on for a single language.
//!
//! `typos` is the case that matters most in practice: it is cross-cutting and on
//! by default, so it is the engine people most often want to silence for one
//! language (a vocabulary-heavy corpus, generated identifiers) while keeping it
//! everywhere else. It is also the sharpest test of the layering, because a
//! cross-cutting backend has *two* tables — the language-agnostic
//! `[lint.typos]` and the per-language `[lint.<lang>.typos]` — and the
//! per-language one has to win, or "off globally, on for Python" is
//! inexpressible.
//!
//! Asserted on diagnostics rather than on the plan: the question a user asks is
//! "did it stop reporting", and a plan that still holds the engine while the
//! engine reports nothing would pass a plan-shaped test while failing theirs.

use std::path::Path;

use poly_core::{Config, LintRun, RunOptions};

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

/// A misspelling in a comment, in two languages, so a per-language switch has
/// something to be per-language about.
fn fixture(dir: &Path) {
    std::fs::write(dir.join("app.py"), "# teh quick brown fox\nx = 1\n").expect("write");
    std::fs::write(dir.join("main.rs"), "// teh quick brown fox\nfn main() {}\n").expect("write");
}

fn lint(dir: &Path, config: &Config) -> LintRun {
    poly_core::lint_run(&[dir.to_path_buf()], config, &options(), false, false).expect("lint run")
}

/// Files that carry at least one `typos` diagnostic, by name.
fn flagged(run: &LintRun) -> Vec<String> {
    let mut names: Vec<String> = run
        .results
        .iter()
        .filter(|result| result.diagnostics.iter().any(|d| d.engine == "typos"))
        .map(|result| result.path.file_name().expect("named").to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn typos_reports_on_every_language_by_default() {
    let dir = tempfile::tempdir().expect("tmp");
    fixture(dir.path());
    assert_eq!(
        flagged(&lint(dir.path(), &Config::default())),
        vec!["app.py", "main.rs"]
    );
}

#[test]
fn typos_can_be_switched_off_globally() {
    let dir = tempfile::tempdir().expect("tmp");
    fixture(dir.path());
    let run = lint(dir.path(), &config("[typos]\nenabled = false\n"));
    assert!(flagged(&run).is_empty(), "got {:?}", flagged(&run));
}

#[test]
fn typos_can_be_switched_off_for_one_language_only() {
    let dir = tempfile::tempdir().expect("tmp");
    fixture(dir.path());
    let run = lint(dir.path(), &config("[python.typos]\nenabled = false\n"));
    assert_eq!(flagged(&run), vec!["main.rs"]);
}

/// The layering, in the direction that is easy to get backwards: off for
/// everything, back on for one language. This fails if the language-agnostic
/// table is read after the per-language one instead of beneath it.
#[test]
fn a_language_can_re_enable_typos_the_global_table_switched_off() {
    let dir = tempfile::tempdir().expect("tmp");
    fixture(dir.path());
    let run = lint(
        dir.path(),
        &config("[typos]\nenabled = false\n\n[python.typos]\nenabled = true\n"),
    );
    assert_eq!(flagged(&run), vec!["app.py"]);
}

/// Switching off a cross-cutting backend must not be mistaken for switching off
/// the language's own one: ruff still runs over Python with typos silenced.
#[test]
fn disabling_typos_leaves_the_language_backend_running() {
    let dir = tempfile::tempdir().expect("tmp");
    std::fs::write(dir.path().join("app.py"), "# teh fox\nimport os\n").expect("write");
    let run = lint(dir.path(), &config("[typos]\nenabled = false\n"));
    assert!(
        run.results
            .iter()
            .any(|result| result.diagnostics.iter().any(|d| d.engine == "ruff")),
        "ruff must still report: {:?}",
        run.results
    );
}
