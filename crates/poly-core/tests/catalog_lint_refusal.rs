//! What a user hears when an enabled `[tools.<name>]` linter is refused.
//!
//! poly declines to run a catalog tool as a linter when the command it would run
//! rewrites files — `sqruff fix`, `rubocop --autocorrect`, `pyupgrade` (which has
//! no read-only mode at all). That refusal is right: a fix command run as a lint
//! pass overwrites the user's source and still exits zero.
//!
//! Saying nothing about it is not. The user enabled that linter, poly runs
//! nothing for it, and the run reports success — a green result standing in for
//! work that was never done. These tests pin the warning, because the run exits 0
//! either way: only the message distinguishes "clean" from "not checked".

use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};

use poly_core::{Config, RunOptions};

/// A `tracing` writer that appends to a shared buffer, so a test can read back
/// what a run logged.
#[derive(Clone)]
struct Buffer(Arc<Mutex<Vec<u8>>>);

impl io::Write for Buffer {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().expect("log buffer poisoned").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Buffer {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// What one run yields these tests: everything logged at `warn` or above, and
/// the names of the engines that actually ran over the file.
struct Run {
    warnings: String,
    engines: Vec<String>,
}

/// Lint `dir` with `config`, capturing its warnings and the engines it ran.
///
/// The subscriber is thread-local (`with_default`), which is enough: engine
/// planning — where the refusal is decided and reported — happens on this thread,
/// before the run fans out over rayon.
fn lint_capturing(dir: &Path, config: &Config) -> Run {
    let buffer = Buffer(Arc::new(Mutex::new(Vec::new())));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(buffer.clone())
        .with_max_level(tracing::Level::WARN)
        .without_time()
        .with_ansi(false)
        .finish();
    let options = RunOptions {
        no_cache: true,
        jobs: Some(1),
        explicit_config: true,
        ..RunOptions::default()
    };
    let results = tracing::subscriber::with_default(subscriber, || {
        poly_core::lint(&[dir.to_path_buf()], config, &options, false, true).expect("lint run")
    });
    let engines = results
        .iter()
        .flat_map(|result| result.debug.iter())
        .flat_map(|debug| debug.engines.iter())
        .map(|engine| engine.engine.clone())
        .collect();
    let logged = buffer.0.lock().expect("log buffer poisoned").clone();
    Run {
        warnings: String::from_utf8(logged).expect("log output is utf-8"),
        engines,
    }
}

/// Write `poly.toml` plus one source file into a fresh directory and lint it.
fn lint_with_tools(tools: &str, file: &str, contents: &str) -> Run {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("poly.toml");
    std::fs::write(&config_path, tools).expect("write poly.toml");
    std::fs::write(dir.path().join(file), contents).expect("write fixture");
    let config = Config::load_file(&config_path).expect("load poly.toml");
    lint_capturing(dir.path(), &config)
}

/// The catalog's `sqruff` lint command is `sqruff fix $PATH` — a mutating
/// subcommand — so no linter runs. The user must be told which tool was dropped,
/// why, and that `poly fmt` still runs it.
#[test]
fn a_mutating_lint_command_is_reported_not_silently_dropped() {
    let run = lint_with_tools("[tools.sqruff]\nenabled = true\n", "q.sql", "select 1\n");
    let logs = run.warnings;

    assert!(logs.contains("sqruff"), "the refused tool must be named, got: {logs:?}");
    assert!(
        logs.contains("sqruff fix"),
        "the warning must quote the mutating command it refused to run, got: {logs:?}"
    );
    assert!(
        logs.contains("cannot run as a linter"),
        "the warning must give the reason, got: {logs:?}"
    );
    assert!(
        logs.contains("poly fmt"),
        "the warning must point at the remedy, got: {logs:?}"
    );
}

/// The other refusal reason: `pyupgrade` rewrites in place whatever the argv, so
/// it is refused by name rather than by argv inspection. Same duty to report.
#[test]
fn an_always_mutating_tool_is_reported_too() {
    let logs = lint_with_tools("[tools.pyupgrade]\nenabled = true\n", "m.py", "x = 1\n").warnings;

    assert!(
        logs.contains("pyupgrade"),
        "the refused tool must be named, got: {logs:?}"
    );
    assert!(
        logs.contains("no check-only mode"),
        "the warning must give the reason, got: {logs:?}"
    );
    assert!(
        logs.contains("poly fmt"),
        "the warning must point at the remedy, got: {logs:?}"
    );
}

/// The negative half, and the reason this is a warning rather than a blanket
/// note: `shellcheck`'s catalog command is read-only, so it wires as a linter and
/// nothing is refused. A warning here would be noise on a run that lost nothing.
///
/// The engine list is asserted alongside the silence so the test cannot pass by
/// the tool having been dropped for some *other* reason — the exact failure it
/// exists to catch.
#[test]
fn an_accepted_lint_command_warns_about_nothing() {
    let run = lint_with_tools(
        "[tools.shellcheck]\nenabled = true\n\n[lint.shell.shellcheck]\nenabled = false\n",
        "s.sh",
        "echo hi\n",
    );

    assert!(
        run.engines.iter().any(|engine| engine == "shellcheck"),
        "the catalog shellcheck linter must have run, got engines: {:?}",
        run.engines
    );
    assert!(
        !run.warnings.contains("cannot run as a linter"),
        "shellcheck lints without rewriting; nothing was refused, got: {:?}",
        run.warnings
    );
}
