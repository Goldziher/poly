//! End-to-end pipeline tests for the core: discovery → routing → run → cache.

use std::fs;

use polylint_core::{Config, RunOptions};

fn write(dir: &std::path::Path, name: &str, content: &str) -> std::path::PathBuf {
    let p = dir.join(name);
    fs::write(&p, content).unwrap();
    p
}

#[test]
fn lint_flags_trailing_whitespace() {
    // Use a Go file: its NativeToolEngine slot delegates `lint` to the
    // tree-sitter generic tier, which emits the catch-all trailing-whitespace
    // diagnostic (gofmt is format-only). (TOML→taplo and YAML→yaml are native
    // backends that do not.) The lint is purely textual, so no grammar download
    // happens here.
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.go", "package main   \nfunc main() {}\n");
    let cfg = Config::default();
    let opts = RunOptions {
        no_cache: true,
        jobs: Some(1),
    };
    let results =
        polylint_core::lint(&[dir.path().to_path_buf()], &cfg, &opts, false, false).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].diagnostics.len(), 1);
    assert_eq!(
        results[0].diagnostics[0].code.as_deref(),
        Some("trailing-whitespace")
    );
}

#[test]
fn format_check_does_not_write_but_reports_change() {
    let dir = tempfile::tempdir().unwrap();
    let messy = "x = 1   \n\n\n";
    let path = write(dir.path(), "a.toml", messy);
    let cfg = Config::default();
    let opts = RunOptions {
        no_cache: true,
        jobs: Some(1),
    };

    let results =
        polylint_core::format(&[dir.path().to_path_buf()], &cfg, &opts, false, false).unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].changed, "check mode should detect a change");
    // File must be untouched in check mode.
    assert_eq!(fs::read_to_string(&path).unwrap(), messy);
}

#[test]
fn format_write_is_idempotent() {
    // Use a YAML file to test whitespace-engine idempotency (trailing ws +
    // blank lines normalized to one trailing newline). TOML now routes to the
    // taplo native backend which has different blank-line semantics.
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "a.yaml", "key: value   \n\n\n");
    let cfg = Config::default();
    let opts = RunOptions {
        no_cache: true,
        jobs: Some(1),
    };

    let first =
        polylint_core::format(&[dir.path().to_path_buf()], &cfg, &opts, true, false).unwrap();
    assert!(first[0].changed);
    let after = fs::read_to_string(&path).unwrap();
    assert_eq!(
        after, "key: value\n",
        "trailing ws + blank lines normalized"
    );

    // Second pass: nothing left to change.
    let second =
        polylint_core::format(&[dir.path().to_path_buf()], &cfg, &opts, true, false).unwrap();
    assert!(!second[0].changed, "formatting must be idempotent");
}

#[test]
fn lint_fix_applies_autofixes_and_dry_run_does_not() {
    // The misspellings live in an excluded fixture so the `typos` pre-commit
    // hook cannot "correct" this test's source out from under it.
    let bad = include_str!("fixtures/typos/known_bad.txt");
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "notes.md", bad);
    let cfg = Config::default();
    let opts = RunOptions {
        no_cache: true,
        jobs: Some(1),
    };

    // Dry run (fix = false) must not touch the file on disk.
    polylint_core::lint(&[dir.path().to_path_buf()], &cfg, &opts, false, false).unwrap();
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        bad,
        "dry run must not modify files"
    );

    // fix = true applies the single-correction typo autofixes in place.
    polylint_core::lint(&[dir.path().to_path_buf()], &cfg, &opts, true, false).unwrap();
    let fixed = fs::read_to_string(&path).unwrap();
    assert_ne!(fixed, bad, "fix must rewrite the file");
    // Assert the exact corrected output. It uses only correctly-spelled words,
    // so the `typos` pre-commit hook cannot rewrite this source and silently
    // break the assertion (the four misspellings stay in the excluded fixture).
    assert_eq!(
        fixed, "The language of the receive function.\nThis is the occurrence of a typo.\n",
        "all four single-correction typos should be autofixed in place",
    );
}

#[test]
fn cache_round_trips() {
    use poly_cache::{Namespace, ResultCache};
    let dir = tempfile::tempdir().unwrap();
    let cache = ResultCache::open(dir.path().join("cache"), true).unwrap();
    let opts = toml::Table::new();
    let digest = ResultCache::single_file_digest("some content");
    let key = ResultCache::key(Namespace::Fmt, "test", "1", &opts, &digest);
    cache.put(Namespace::Fmt, &key, b"formatted").unwrap();
    assert_eq!(
        cache.get(Namespace::Fmt, &key).as_deref(),
        Some(&b"formatted"[..])
    );
    // A different version yields a different key (invalidation).
    let key2 = ResultCache::key(Namespace::Fmt, "test", "2", &opts, &digest);
    assert_ne!(key, key2);
}
