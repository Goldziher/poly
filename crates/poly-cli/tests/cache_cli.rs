//! End-to-end coverage for `poly cache` driving the result-cache maintenance
//! API over an explicit `--cache-dir`.
//!
//! These shell out to the built `poly` binary (via `CARGO_BIN_EXE_poly`) and
//! seed a cache tree on disk, so they exercise the full path: arg parsing →
//! root resolution → `ResultCache` maintenance → human output → exit code.
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

const POLY: &str = env!("CARGO_BIN_EXE_poly");

/// Seed a cache tree with `entries` files (each `bytes` long) in `results/hook`.
fn seed_cache(cache_dir: &Path, entries: usize, bytes: usize) {
    let hook = cache_dir.join("results").join("hook");
    std::fs::create_dir_all(&hook).expect("create results/hook");
    std::fs::create_dir_all(cache_dir.join("results").join("lint")).expect("create results/lint");
    std::fs::create_dir_all(cache_dir.join("results").join("fmt")).expect("create results/fmt");
    std::fs::write(cache_dir.join("VERSION"), "1").expect("write VERSION");
    for index in 0..entries {
        std::fs::write(hook.join(format!("entry{index}")), vec![b'x'; bytes]).expect("write entry");
    }
}

fn poly_cache(cache_dir: &Path, args: &[&str]) -> Output {
    Command::new(POLY)
        .arg("cache")
        .arg("--cache-dir")
        .arg(cache_dir)
        .args(args)
        .output()
        .expect("poly invocation")
}

#[test]
fn stats_reports_entries_for_a_populated_cache() {
    let tmp = TempDir::new().expect("tempdir");
    let cache_dir = tmp.path().join("cache");
    seed_cache(&cache_dir, 3, 100);

    let output = poly_cache(&cache_dir, &["stats"]);
    assert!(output.status.success(), "stats should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("entries"), "stats output: {stdout}");
    assert!(stdout.contains("total: 3 entries"), "stats output: {stdout}");
}

#[test]
fn size_prints_a_byte_count() {
    let tmp = TempDir::new().expect("tempdir");
    let cache_dir = tmp.path().join("cache");
    seed_cache(&cache_dir, 2, 50);

    let output = poly_cache(&cache_dir, &["size"]);
    assert!(output.status.success(), "size should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let total: u64 = stdout.trim().parse().expect("size prints a bare number");
    assert_eq!(total, 100, "two 50-byte entries");
}

#[test]
fn clean_empties_the_cache() {
    let tmp = TempDir::new().expect("tempdir");
    let cache_dir = tmp.path().join("cache");
    seed_cache(&cache_dir, 4, 25);

    let clean = poly_cache(&cache_dir, &["clean"]);
    assert!(clean.status.success(), "clean should succeed");

    let size = poly_cache(&cache_dir, &["size"]);
    assert!(size.status.success());
    let total: u64 = String::from_utf8_lossy(&size.stdout)
        .trim()
        .parse()
        .expect("size prints a bare number");
    assert_eq!(total, 0, "clean must empty the cache");
}

#[test]
fn default_subcommand_is_stats() {
    let tmp = TempDir::new().expect("tempdir");
    let cache_dir = tmp.path().join("cache");
    seed_cache(&cache_dir, 1, 10);

    let output = poly_cache(&cache_dir, &[]);
    assert!(output.status.success(), "default (stats) should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("cache format version"), "output: {stdout}");
}
