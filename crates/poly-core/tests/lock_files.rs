//! Package-manager lock files are never linted, matching what `poly migrate` says. The format half
//! (a lock file is left alone while its YAML neighbour is rewritten) needs the web family's formatter and
//! lives in `components/crates/poly-engines-web/tests/lock_files.rs`.

use std::fs;

use poly_core::{Config, LOCKFILE_SKIP, RunOptions};
use tempfile::tempdir;

fn options() -> RunOptions {
    RunOptions {
        no_cache: true,
        jobs: Some(1),
        ..RunOptions::default()
    }
}

#[test]
fn a_named_lock_file_is_skipped_by_lint() {
    let repo = tempdir().unwrap();
    let lock = repo.path().join("pnpm-lock.yaml");
    fs::write(&lock, "a: [1, 2\n").unwrap();

    let run = poly_core::lint_run(
        std::slice::from_ref(&lock),
        &Config::default(),
        &options(),
        false,
        false,
    )
    .unwrap();

    assert!(run.results.iter().all(|result| result.diagnostics.is_empty()));
    assert!(
        run.skipped
            .iter()
            .any(|skip| skip.path == lock && skip.reason == LOCKFILE_SKIP)
    );
}
