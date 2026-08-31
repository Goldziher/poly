//! What the config fingerprint is allowed to claim.
//!
//! Two runs of the same binary over the same tree can enforce different rules:
//! a `poly.toml`, a `poly.local.toml`, a nested config or an `extends` base can
//! change underneath an identical executable. Both report clean, and nothing in
//! either says they are not comparable — so a consumer memoizing results on the
//! binary's identity alone is wrong the moment config moves without a version
//! bump.
//!
//! The properties that make the hash usable are all negative ones: it must not
//! move when nothing meaningful changed, and it must not depend on where the
//! repository happens to sit on disk.

use std::path::Path;

use poly_core::{Config, LintRun, RunOptions};

fn options() -> RunOptions {
    RunOptions {
        no_cache: true,
        jobs: Some(1),
        ..RunOptions::default()
    }
}

fn write(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(path, body).expect("write");
}

fn run(dir: &Path) -> LintRun {
    poly_core::lint_run(&[dir.to_path_buf()], &Config::default(), &options(), false, false).expect("lint run")
}

/// The one distinct fingerprint a single-config repo produces.
///
/// The set can hold the same config twice — `ConfigSet` keeps the run's root
/// config at index 0 *and* registers the directory that holds it — so this
/// asserts on the distinct hashes, which is what a consumer compares.
fn single_hash(dir: &Path) -> String {
    let run = run(dir);
    let hashes: std::collections::BTreeSet<String> = run.configs.iter().map(|c| c.hash.clone()).collect();
    assert_eq!(hashes.len(), 1, "one config governed this run: {:?}", run.configs);
    let hash = hashes.into_iter().next().expect("one hash");
    assert_ne!(hash, "unresolved", "the config resolved, so it must hash");
    hash
}

fn repo(config: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tmp");
    write(&dir.path().join("poly.toml"), config);
    write(&dir.path().join("app.py"), "x = 1\n");
    dir
}

#[test]
fn a_config_change_moves_the_hash() {
    let before = single_hash(repo("[defaults]\nline_length = 120\n").path());
    let after = single_hash(repo("[defaults]\nline_length = 100\n").path());
    assert_ne!(before, after, "a changed rule must be visible in the fingerprint");
}

/// Comments and blank lines are not configuration. If they moved the hash, every
/// cosmetic edit would look like a rule change and the signal would be useless.
#[test]
fn formatting_only_edits_do_not_move_the_hash() {
    let plain = single_hash(repo("[defaults]\nline_length = 120\n").path());
    let commented = single_hash(
        repo("# our house style\n\n[defaults]\n\n# 120 is the project-wide width\nline_length = 120\n").path(),
    );
    assert_eq!(plain, commented, "only the resolved values matter");
}

/// The hazard that would have made this unusable in CI: the resolver rewrites
/// `[rules] dirs` to **absolute** paths, so hashing the table verbatim would
/// make the digest depend on the checkout location and two identical commits
/// would never agree.
#[test]
fn the_hash_does_not_depend_on_where_the_repository_sits() {
    let config = "[rules]\ndirs = [\".poly/rules\"]\n";
    let first = repo(config);
    let second = repo(config);
    write(&first.path().join(".poly/rules/.keep"), "");
    write(&second.path().join(".poly/rules/.keep"), "");
    assert_ne!(first.path(), second.path(), "two different checkout locations");
    assert_eq!(
        single_hash(first.path()),
        single_hash(second.path()),
        "the same config in two directories is the same config"
    );
}

/// A monorepo run carries one entry per governing config, each naming the
/// directory it resolved from, so a difference between sibling packages is
/// attributable rather than anomalous.
#[test]
fn a_monorepo_run_fingerprints_each_config_and_names_its_root() {
    let dir = tempfile::tempdir().expect("tmp");
    write(&dir.path().join("poly.toml"), "[defaults]\nline_length = 120\n");
    write(&dir.path().join("app.py"), "x = 1\n");
    write(
        &dir.path().join("packages/api/poly.toml"),
        "[defaults]\nline_length = 80\n",
    );
    write(&dir.path().join("packages/api/main.py"), "y = 2\n");

    let run = run(dir.path());
    assert!(
        run.configs.len() >= 2,
        "the nested config is its own entry: {:?}",
        run.configs
    );
    let hashes: std::collections::BTreeSet<&str> = run.configs.iter().map(|c| c.hash.as_str()).collect();
    assert!(
        hashes.len() >= 2,
        "the two configs differ, so their fingerprints must: {:?}",
        run.configs
    );
    assert!(
        run.configs.iter().any(|c| c.root.as_deref() == Some("packages/api")),
        "the nested root is named, and relative to the run: {:?}",
        run.configs
    );

    // Every result points at the config that governed it, so attribution needs
    // no prefix-matching by the consumer.
    for result in &run.results {
        assert!(result.config < run.configs.len(), "result names a real config");
    }
}
