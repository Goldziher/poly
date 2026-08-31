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

// --- Formatting: switching a formatter off must not remove the file from the run ---

fn format_run_with(dir: &Path, config: &Config) -> poly_core::FormatRun {
    poly_core::format_run(&[dir.to_path_buf()], config, &options(), false, false).expect("format run")
}

/// Go, Rust, Zig, Java, Kotlin, R, Swift, Dart and Gleam hold **one** registry
/// slot each — `NativeToolEngine` — with no separately registered tree-sitter
/// entry. That engine's `format` capability is unconditionally `true` for
/// exactly this reason, and its `format` delegates to the tier-2 reindenter when
/// it is switched off. So `enabled = false` means "do not shell out to the
/// native tool", never "stop formatting this language" — otherwise one config
/// key silently removes a language from every format run.
#[test]
fn disabling_a_native_tool_formatter_falls_back_to_tier_two() {
    let dir = tempfile::tempdir().expect("tmp");
    std::fs::write(dir.path().join("main.rs"), "fn main() {\nlet x = 1;\n}\n").expect("write");
    let config = Config {
        fmt: toml::from_str("[rust.rustfmt]\nenabled = false\n").expect("valid fmt config"),
        ..Config::default()
    };
    let run = format_run_with(dir.path(), &config);
    let result = run
        .results
        .iter()
        .find(|r| r.path.file_name().is_some_and(|f| f == "main.rs"))
        .expect("main.rs is in the run");
    assert!(result.error.is_none(), "unexpected error: {:?}", result.error);
    assert!(
        result.changed,
        "the tier-2 reindenter must still format the file: {result:?}"
    );
}

/// The other half: when a withdrawal *does* leave a file with no formatter, the
/// run says so. An empty format plan used to be unreachable, so it was treated
/// as ordinary coverage and reported nothing — which turns a narrowed run into
/// "All formatted" over files nothing touched.
#[test]
fn a_file_left_with_no_formatter_is_reported_rather_than_passed_over() {
    let dir = tempfile::tempdir().expect("tmp");
    std::fs::write(dir.path().join("app.py"), "x = 1\n").expect("write");
    std::fs::write(dir.path().join("style.css"), "a{color:red}\n").expect("write");
    let run = poly_core::format_run(
        &[dir.path().to_path_buf()],
        &Config::default(),
        &RunOptions {
            only: vec!["ruff".to_string()],
            ..options()
        },
        false,
        false,
    )
    .expect("format run");
    let skipped: Vec<String> = run
        .skipped
        .iter()
        .map(|s| s.path.file_name().expect("named").to_string_lossy().into_owned())
        .collect();
    assert!(
        skipped.contains(&"style.css".to_string()),
        "the deselected file must be reported, got {skipped:?}"
    );
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

/// The counterpart: a limitation of poly stays charged, so the exemption above
/// cannot be reached by accident.
#[test]
fn a_language_poly_has_no_rules_for_is_still_charged() {
    let dir = tempfile::tempdir().expect("tmp");
    std::fs::write(dir.path().join("main.zig"), "pub fn main() void {}\n").expect("write");
    let run = lint(dir.path(), &Config::default());
    let reason = run
        .skipped
        .iter()
        .find(|s| s.path.file_name().is_some_and(|f| f == "main.zig"))
        .map(|s| s.reason.clone())
        .expect("the file is reported as skipped");
    assert!(
        !poly_core::is_withdrawal_reason(&reason),
        "a poly limitation must stay charged: {reason:?}"
    );
}
