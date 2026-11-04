//! The biome engines must not spawn threads on the per-call path: inside a component (cdylib) a
//! stray pool would sit outside poly's `-j` budget and its 16 MiB stacks. The analyzers are
//! single-threaded and `biome_fs` is not linked into the call; this pins that.
//!
//! A single `#[test]` on purpose: tests in one binary run on parallel threads, which would make the
//! task count unstable.
#![cfg(target_os = "linux")]

use poly_engine_api::config::{EngineConfig, GlobalDefaults};
use poly_engine_api::engine::{Engine, SourceFile};
use poly_engine_api::language::Language;
use poly_engines_biome::{BiomeCssEngine, BiomeGraphqlEngine};

fn thread_count() -> usize {
    std::fs::read_dir("/proc/self/task")
        .expect("/proc/self/task is readable on Linux")
        .count()
}

fn cfg() -> EngineConfig {
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 2,
        options: toml::Table::new(),
    }
}

#[test]
fn biome_engines_leave_no_threads_behind() {
    let baseline = thread_count();
    let css = SourceFile {
        path: "fixture.css".into(),
        language: Language::Css,
        content: "a { colr: blue; }\n".into(),
    };
    let graphql = SourceFile {
        path: "fixture.graphql".into(),
        language: Language::GraphQl,
        content: "query { user { id } }\n".into(),
    };

    // A call that returns nothing may mean the fixture was not linted at all, which would make the
    // thread assertion vacuous.
    assert!(!BiomeCssEngine.lint(&css, &cfg()).unwrap().is_empty());
    assert!(thread_count() <= baseline, "the CSS engine left a thread behind");
    assert!(!BiomeGraphqlEngine.lint(&graphql, &cfg()).unwrap().is_empty());
    assert!(thread_count() <= baseline, "the GraphQL engine left a thread behind");
}
