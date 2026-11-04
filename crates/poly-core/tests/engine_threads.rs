//! Engines must not spawn threads on the per-call path.
//!
//! Inside a component (cdylib), rayon or a stray thread pool would be a second pool outside poly's
//! `-j` budget and its 16 MiB stacks. This test runs one small source through each in-process lint
//! engine on the test thread and compares `/proc/self/task` against the count taken before any engine
//! ran, after every call.
//!
//! It is a single `#[test]` on purpose: tests in one binary run on parallel threads, which would make
//! the task count unstable.
//!
//! Audit behind the allowlist (see `allowed_growth`): sqruff (`lint_files` rayon, not reached by
//! `lint_string`), mago (`mago-database` loader and the extension worker pool are not reached by
//! `File::ephemeral` + `Linter::lint`), ruff (no rayon or salsa database on the lint path). The biome
//! engines are checked in `poly-engines-biome`'s `tests/threads.rs`; the oxc audit lives with the web
//! family in `components/crates/poly-engines-web/tests/engine_threads.rs`.
#![cfg(all(target_os = "linux", feature = "all-engines"))]

use poly_core::config::{Config, EngineConfig, GlobalDefaults, Kind};
use poly_core::engine::{Engine, SourceFile};
use poly_core::engines::mago::MagoEngine;
use poly_core::engines::ruff::RuffEngine;
use poly_core::engines::sqruff::SqruffEngine;
use poly_core::language::Language;

/// Number of threads in this process.
fn thread_count() -> usize {
    std::fs::read_dir("/proc/self/task")
        .expect("/proc/self/task is readable on Linux")
        .count()
}

/// Threads an engine may leave behind, beyond the count before the first engine call.
///
/// Every engine is currently `0`: none of them reaches rayon or spawns a thread for a single
/// in-memory source. An entry raised above `0` must name the file and line that spawns the thread.
fn allowed_growth(engine: &str) -> usize {
    match engine {
        // `lint_string` is single-threaded; rayon is only in `Linter::lint_files` (core.rs:257).
        "sqruff" => 0,
        // `File::ephemeral` + `Linter::lint` with no external linter; `mago-database` rayon and
        // `mago-extension` worker threads are not reached.
        "mago" => 0,
        // `lint_only` has no rayon, and poly builds no salsa database.
        "ruff" => 0,
        other => panic!("no thread allowance recorded for engine {other}"),
    }
}

fn source(path: &str, language: Language, content: &str) -> SourceFile {
    SourceFile {
        path: path.into(),
        language,
        content: content.into(),
    }
}

fn plain_cfg(indent_width: usize) -> EngineConfig {
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width,
        options: toml::Table::new(),
    }
}

#[test]
fn lint_engines_do_not_grow_the_thread_count() {
    let baseline = thread_count();

    let sqruff_cfg = Config::default().engine_config(&Language::Sql, "sqruff", Kind::Lint);

    // (engine name for the allowlist, diagnostics produced, thread count after the call)
    let mut observed: Vec<(&str, usize, usize)> = Vec::new();
    let mut record = |engine: &'static str, diagnostics: usize| {
        observed.push((engine, diagnostics, thread_count()));
    };

    // Each call runs twice: the first exercises lazy initialisation, the second the cached path.
    for _ in 0..2 {
        let sql = source("fixture.sql", Language::Sql, "SELECT a,b FROM t WHERE a=1\n");
        record("sqruff", SqruffEngine.lint(&sql, &sqruff_cfg).unwrap().len());

        let php = source(
            "fixture.php",
            Language::Php,
            "<?php\nclass Foo {\n    public function bar() { return 1; }\n}\n",
        );
        record("mago", MagoEngine::default().lint(&php, &plain_cfg(4)).unwrap().len());

        let py = source(
            "fixture.py",
            Language::Python,
            "import os\nx = 1\nif x == None:\n    pass\n",
        );
        record("ruff", RuffEngine.lint(&py, &plain_cfg(4)).unwrap().len());
    }

    // A call that returns nothing may mean the fixture was not linted at all, which would make the
    // thread assertion below vacuous; the ruff fixture carries a finding.
    assert!(
        observed
            .iter()
            .any(|(name, diagnostics, _)| *name == "ruff" && *diagnostics > 0),
        "ruff fixture produced no diagnostics, so the engine may not have run"
    );

    for (engine, _, threads) in &observed {
        let allowed = baseline + allowed_growth(engine);
        assert!(
            *threads <= allowed,
            "{engine} left {} extra thread(s) (baseline {baseline}, now {threads}, allowed {})",
            threads - baseline,
            allowed_growth(engine),
        );
    }
    assert!(
        thread_count() <= baseline,
        "threads remain after the engine calls (baseline {baseline}, now {})",
        thread_count()
    );
}
