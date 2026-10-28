//! Package-manager lock files are never formatted or linted, matching what `poly migrate` says.

use std::fs;

use poly_core::{Config, LOCKFILE_SKIP, RunOptions};
use tempfile::tempdir;

const LOOSE_YAML: &str = "a:     1\nb:   [1,2]\n";

fn options() -> RunOptions {
    RunOptions {
        no_cache: true,
        jobs: Some(1),
        ..RunOptions::default()
    }
}

#[test]
fn lock_files_are_not_formatted_but_their_neighbours_are() {
    let repo = tempdir().unwrap();
    fs::write(repo.path().join("pnpm-lock.yaml"), LOOSE_YAML).unwrap();
    fs::write(repo.path().join("values.yaml"), LOOSE_YAML).unwrap();

    poly_core::format_run(
        &[repo.path().to_path_buf()],
        &Config::default(),
        &options(),
        true,
        false,
    )
    .unwrap();

    assert_eq!(
        fs::read_to_string(repo.path().join("pnpm-lock.yaml")).unwrap(),
        LOOSE_YAML
    );
    assert_ne!(fs::read_to_string(repo.path().join("values.yaml")).unwrap(), LOOSE_YAML);
}

#[test]
fn a_named_lock_file_is_skipped_by_lint() {
    let repo = tempdir().unwrap();
    let lock = repo.path().join("pnpm-lock.yaml");
    fs::write(&lock, "a: [1, 2\n").unwrap();

    let run = poly_core::lint_run(std::slice::from_ref(&lock), &Config::default(), &options(), false, false).unwrap();

    assert!(run.results.iter().all(|result| result.diagnostics.is_empty()));
    assert!(
        run.skipped
            .iter()
            .any(|skip| skip.path == lock && skip.reason == LOCKFILE_SKIP)
    );
}
