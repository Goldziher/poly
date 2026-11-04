//! The oxc lint engine must not spawn threads on the per-call path.
//!
//! Inside a component (cdylib), rayon or a stray thread pool would be a second pool outside poly's
//! `-j` budget and its 16 MiB stacks. This test runs small sources through the oxc lint engine on
//! the test thread and compares `/proc/self/task` against the count taken before any call, after
//! every call.
//!
//! Audit: oxc (`LintService::run_source` used a `rayon::scope`, now replaced by `src/oxc/run.rs`,
//! which drives `Linter::run` directly with no `LintService`, so no rayon scope and no global pool:
//! `Runtime::new` builds one with `ThreadPoolBuilder::build_global`).
//!
//! It is a single `#[test]` on purpose: tests in one binary run on parallel threads, which would make
//! the task count unstable.
#![cfg(target_os = "linux")]

use poly_engine_api::config::{EngineConfig, GlobalDefaults};
use poly_engine_api::engine::{Engine, SourceFile};
use poly_engine_api::language::Language;
use poly_engines_web::OxcEngine;

/// Number of threads in this process.
fn thread_count() -> usize {
    std::fs::read_dir("/proc/self/task")
        .expect("/proc/self/task is readable on Linux")
        .count()
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
fn oxc_lint_does_not_grow_the_thread_count() {
    let baseline = thread_count();

    let oxc_error_cfg = EngineConfig {
        options: toml::from_str("[rules.no-debugger]\nlevel = \"error\"\n").unwrap(),
        ..plain_cfg(2)
    };

    // (diagnostics produced, thread count after the call)
    let mut observed: Vec<(usize, usize)> = Vec::new();
    let mut record = |diagnostics: usize| {
        observed.push((diagnostics, thread_count()));
    };

    // Each call runs twice: the first exercises lazy initialisation, the second the cached path.
    for _ in 0..2 {
        let js = source(
            "fixture.js",
            Language::JavaScript,
            "const x = 1;\ndebugger;\nfoo: for (;;) { break; }\n",
        );
        record(OxcEngine.lint(&js, &plain_cfg(2)).unwrap().len());

        let ts = source(
            "fixture.ts",
            Language::TypeScript,
            "export const f = (a: number): number => a == 1 ? 1 : 2;\n",
        );
        record(OxcEngine.lint(&ts, &plain_cfg(2)).unwrap().len());

        // A non-empty option table takes the per-config cached linter path.
        record(OxcEngine.lint(&js, &oxc_error_cfg).unwrap().len());

        // A parse error takes the diagnostics-only path.
        let broken = source(
            "broken.js",
            Language::JavaScript,
            "const x = {\n  a: 1,\nconst y = 2;\n",
        );
        record(OxcEngine.lint(&broken, &plain_cfg(2)).unwrap().len());
    }

    // A call that returns nothing may mean the fixture was not linted at all, which would make the
    // thread assertion below vacuous.
    assert!(
        observed.iter().any(|(diagnostics, _)| *diagnostics > 0),
        "the oxc fixtures produced no diagnostics, so the engine may not have run"
    );

    for (_, threads) in &observed {
        assert!(
            *threads <= baseline,
            "oxc left {} extra thread(s) (baseline {baseline}, now {threads})",
            threads - baseline,
        );
    }
    assert!(
        thread_count() <= baseline,
        "threads remain after the engine calls (baseline {baseline}, now {})",
        thread_count()
    );
}
