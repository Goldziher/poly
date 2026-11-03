//! The real `sql` component, built by alef and loaded from a `file://` dev lock, against the same
//! engine linked in-process.
//!
//! Runs only when `POLY_COMPONENT_DEV_LOCK` names a lock written by `mise run components:dev`;
//! otherwise it reports that it skipped and passes, because building the component compiles a
//! cdylib that a plain `cargo test` has no reason to do.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use poly_component_host::{ComponentHost, HostOptions, SlotStatus};
use poly_component_protocol::decode_describe;
use poly_engine_api::config::{EngineConfig, GlobalDefaults};
use poly_engine_api::engine::{Diagnostic, Engine, SourceFile};
use poly_engine_api::language::Language;
use poly_engines::sql::SqruffEngine;
use poly_engines::{PolyComponent, SqlComponent};
use rayon::prelude::*;

const DEV_LOCK_ENV: &str = "POLY_COMPONENT_DEV_LOCK";
const COMPONENT_ID: &str = "sql";
const WORKERS: usize = 8;
const FILES: usize = 64;
const WORKER_STACK_SIZE: usize = 16 * 1024 * 1024;

/// Known-bad SQL from `crates/poly-engines-sql/tests/sqruff.rs`: an implicit alias (AL02) and
/// `= null` (CV05).
const KNOWN_BAD: &str = "select a b, id,name from users where id = null\n";

fn config() -> EngineConfig {
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: Language::Sql.default_indent_width(),
        options: toml::Table::new(),
    }
}

fn source(content: &str) -> SourceFile {
    SourceFile {
        path: PathBuf::from("test.sql"),
        language: Language::Sql,
        content: Arc::from(content),
    }
}

fn encoded(diagnostics: &[Diagnostic]) -> Vec<u8> {
    serde_json::to_vec(diagnostics).expect("diagnostics serialize")
}

fn dev_lock() -> Option<String> {
    let path = std::env::var(DEV_LOCK_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())?;
    let lock = std::fs::read_to_string(Path::new(&path)).unwrap_or_else(|e| panic!("reading {path}: {e}"));
    Some(lock)
}

#[test]
fn the_sql_component_lints_like_the_in_process_engine() {
    let Some(lock) = dev_lock() else {
        eprintln!("skipped: {DEV_LOCK_ENV} is not set; run `mise run components:dev` first");
        return;
    };
    let cache = tempfile::tempdir().unwrap();
    let host = ComponentHost::new(HostOptions::new(lock, cache.path())).expect("host over the dev lock");

    let describe = decode_describe(&SqlComponent::default().describe().unwrap()).expect("in-process describe decodes");
    let wire = describe
        .engines
        .first()
        .expect("the sql component hosts an engine")
        .clone();
    assert_eq!(wire.name, "sqruff");
    let engine = host
        .engine(COMPONENT_ID, &wire, Some(describe))
        .expect("engine proxy over the component");
    assert_eq!(
        engine.slot().status(),
        SlotStatus::Unloaded,
        "nothing loads before first use"
    );

    let cfg = config();
    let src = source(KNOWN_BAD);
    let expected = SqruffEngine.lint(&src, &cfg).expect("in-process lint");
    assert!(!expected.is_empty(), "the fixture must produce diagnostics");
    let expected = encoded(&expected);

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(WORKERS)
        .stack_size(WORKER_STACK_SIZE)
        .build()
        .unwrap();
    let results: Vec<Vec<u8>> = pool.install(|| {
        (0..FILES)
            .into_par_iter()
            .map(|_| encoded(&engine.lint(&src, &cfg).expect("component lint")))
            .collect()
    });

    assert_eq!(results.len(), FILES);
    for (index, result) in results.iter().enumerate() {
        assert_eq!(
            result, &expected,
            "file {index} differs from the in-process diagnostics"
        );
    }
    assert_eq!(engine.slot().status(), SlotStatus::Ready);
}
