//! Component parity: the sql component, installed from the dev lock and driven through the full
//! runner, answers byte-for-byte what the in-process sqruff engine answers.
//!
//! The reference is the insta snapshots `poly-engines-sql` commits for its in-process
//! `SqruffEngine` tests, read straight from that crate's tree, so a change to either side that
//! breaks parity fails here without a second copy of the expectations to keep in sync.
//!
//! Only meaningful when `engine-sql` is off (the sql family then comes from a component) and
//! `dev-lock` is on. In that configuration an unset `POLY_COMPONENT_DEV_LOCK` is a failure, not a
//! skip: run `mise run components:dev` first, or `mise run components:parity`.
#![cfg(all(feature = "dev-lock", not(feature = "engine-sql")))]

use std::path::{Path, PathBuf};

use poly_component_host::{ComponentHost, HostOptions};
use poly_core::components::{ComponentInjection, LOCK_ENV};
use poly_core::{Config, RunOptions};

const KNOWN_BAD: &str = "select a b, id,name from users where id = null\n";
const KNOWN_UNFORMATTED: &str = "select id , name from  users where id=1\n";

fn lock_path() -> PathBuf {
    let Some(path) = std::env::var_os(LOCK_ENV) else {
        panic!(
            "{LOCK_ENV} is not set: run `mise run components:dev` and export the lock it prints (or `mise run components:parity`)"
        );
    };
    PathBuf::from(path)
}

fn options() -> RunOptions {
    RunOptions {
        no_cache: true,
        jobs: Some(1),
        explicit_config: true,
        ..RunOptions::default()
    }
}

/// The body of a committed insta snapshot, front matter removed.
fn snapshot(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../poly-engines-sql/tests/snapshots")
        .join(format!("sqruff__{name}.snap"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let mut parts = text.splitn(3, "---\n");
    assert_eq!(parts.next(), Some(""), "{} has no front matter", path.display());
    parts.next().expect("front matter");
    parts.next().expect("snapshot body").trim_end().to_owned()
}

fn project(content: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("test.sql"), content).unwrap();
    dir
}

#[test]
fn lint_through_the_component_matches_the_in_process_snapshot() {
    lock_path();
    let dir = project(KNOWN_BAD);
    let run = poly_core::lint_run(
        &[dir.path().to_path_buf()],
        &Config::default(),
        &options(),
        false,
        false,
    )
    .unwrap();
    assert_eq!(run.components.used, ["sql"], "the sql component must serve the run");
    assert!(run.components.missing.is_empty(), "{:?}", run.components.missing);
    assert!(run.errors.is_empty(), "{:?}", run.errors);
    let diagnostics: Vec<_> = run
        .results
        .iter()
        .flat_map(|r| r.diagnostics.iter())
        .filter(|d| d.engine == "sqruff")
        .collect();
    assert_eq!(format!("{diagnostics:#?}"), snapshot("sqruff_known_bad"));
}

#[test]
fn format_through_the_component_matches_the_in_process_snapshot() {
    lock_path();
    let dir = project(KNOWN_UNFORMATTED);
    let run = poly_core::format_run(
        &[dir.path().to_path_buf()],
        &Config::default(),
        &options(),
        false,
        false,
    )
    .unwrap();
    assert_eq!(run.components.used, ["sql"], "the sql component must serve the run");
    assert!(run.errors.is_empty(), "{:?}", run.errors);
    let formatted = run
        .results
        .iter()
        .find_map(|r| r.formatted.as_deref())
        .expect("the component formatted the known-unformatted file");
    assert_eq!(formatted.trim_end(), snapshot("sqruff_known_unformatted"));
}

#[test]
fn a_component_with_the_wrong_hash_blocks_the_run_as_verify_failed() {
    let text = std::fs::read_to_string(lock_path()).expect("read the dev lock");
    let mut lock: serde_json::Value = serde_json::from_str(&text).expect("the dev lock is JSON");
    let artifacts = lock["artifacts"].as_array_mut().expect("artifacts");
    assert!(!artifacts.is_empty(), "the dev lock lists no artifact");
    for artifact in artifacts {
        artifact["sha256"] = serde_json::Value::String("00".repeat(32));
    }
    let cache = tempfile::tempdir().unwrap();
    let mut host_options = HostOptions::new(lock.to_string(), cache.path().to_path_buf());
    host_options.gate_loads = true;
    let host = ComponentHost::new(host_options).unwrap();
    let injected = RunOptions {
        component_injection: Some(ComponentInjection::new(host, poly_component_index::index().clone())),
        ..options()
    };

    let dir = project(KNOWN_BAD);
    let run = poly_core::lint_run(&[dir.path().to_path_buf()], &Config::default(), &injected, false, false).unwrap();
    assert_eq!(run.components.missing.len(), 1, "{:?}", run.components.missing);
    assert_eq!(run.components.missing[0].kind, "verify_failed");
    assert!(run.components.blocking(), "a hash mismatch must fail the run (exit 2)");
    assert!(!run.errors.is_empty(), "the file must be an error, not a pass");
}
