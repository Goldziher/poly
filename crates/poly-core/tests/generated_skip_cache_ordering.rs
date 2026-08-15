//! Pins the ordering in `runner.rs`'s `format_one`: the hash-stamped generated
//! file skip must run **before** the fmt result-cache lookup.
//!
//! That ordering is why a stamp-detection fix (e.g. the v0.21.5 fix that
//! taught `is_hash_stamped_source` to look past a frontmatter/licence
//! preamble) takes effect on the very first run after upgrading, regardless
//! of what a pre-fix binary already cached. If the skip were checked *after*
//! the cache lookup, a consumer upgrading to a fixed `poly` could get a cache
//! hit computed by the pre-fix binary and "verify" the fix against a result
//! the fixed code never actually touched.
//!
//! The cache lives in a per-user OS cache dir by default, so this test points
//! `POLY_CACHE_HOME` at a private temp directory for the duration of the
//! test (serialized against any other test in this binary that also touches
//! process environment, since `std::env::set_var` is process-global).

use std::fs;
use std::sync::Mutex;

use poly_core::{Config, RunOptions};

/// Serializes tests in this binary that mutate `POLY_CACHE_HOME` (a
/// process-global environment variable); `cargo test` runs tests within one
/// binary in parallel by default.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn write(dir: &std::path::Path, name: &str, content: &str) -> std::path::PathBuf {
    let p = dir.join(name);
    fs::write(&p, content).unwrap();
    p
}

#[test]
fn generated_skip_outcome_is_independent_of_cache_state() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let cache_home = tempfile::tempdir().unwrap();
    // SAFETY: serialized by `ENV_LOCK` for the whole test, and removed again
    // before the lock is released, so no other test in this binary observes
    // the override.
    unsafe {
        std::env::set_var("POLY_CACHE_HOME", cache_home.path());
    }

    let dir = tempfile::tempdir().unwrap();
    // A real structured hash stamp — `<project>:hash:<8+ hex>`, the exact
    // shape `is_hash_stamped_source` (crates/poly-core/src/filter.rs) looks
    // for — on a TOML file taplo will reformat (irregular spacing, blank
    // trailing lines).
    let stamped = "# alef:hash:0123456789abcdef\nx  =   1\n\n\n";
    let path = write(dir.path(), "a.toml", stamped);
    let cfg = Config::default();
    let warm_up_opts = RunOptions {
        no_cache: false,
        jobs: Some(1),
        explicit_config: true,
        fix_generated: true,
        ..RunOptions::default()
    };

    // Warm-up #1: `fix_generated` bypasses the skip, so this formats the
    // stamped file and caches a result keyed on its digest. `write: false`
    // keeps the on-disk content stable across both warm-up runs — only the
    // cache changes between them.
    let warm_up = poly_core::format(&[dir.path().to_path_buf()], &cfg, &warm_up_opts, false, true).unwrap();
    assert_eq!(warm_up.len(), 1, "expected exactly one discovered file");
    assert!(
        warm_up[0].changed,
        "fixture must actually need reformatting, or nothing gets cached and the rest of this test proves nothing"
    );
    let ran_uncached = warm_up[0]
        .debug
        .as_ref()
        .expect("collect_debug was requested")
        .engines
        .iter()
        .any(|engine| !engine.cache_hit);
    assert!(
        ran_uncached,
        "warm-up #1 must be an uncached run, or warm-up #2 proves nothing"
    );

    // Warm-up #2: same (untouched) on-disk content, so the same digest — this
    // must now be a cache hit. Non-vacuous proof that warm-up #1 actually
    // populated the fmt cache, not just that it ran.
    let confirm = poly_core::format(&[dir.path().to_path_buf()], &cfg, &warm_up_opts, false, true).unwrap();
    let cache_hit = confirm[0]
        .debug
        .as_ref()
        .expect("collect_debug was requested")
        .engines
        .iter()
        .any(|engine| engine.cache_hit);
    assert!(
        cache_hit,
        "warm-up did not populate the fmt cache: the ordering guard below would pass vacuously"
    );

    // The regression itself: `fix_generated: false` now, with the cache
    // warm — and `write: true`, so a wrongly-ordered pipeline that read the
    // cached (formatted) bytes back would actually overwrite the file. If the
    // generated-file skip ran after the cache lookup instead of before it,
    // this call would report the file as fixed instead of skipped.
    let real_opts = RunOptions {
        fix_generated: false,
        ..warm_up_opts
    };
    let result = poly_core::format(&[dir.path().to_path_buf()], &cfg, &real_opts, true, false).unwrap();

    unsafe {
        std::env::remove_var("POLY_CACHE_HOME");
    }

    assert_eq!(result.len(), 1, "expected exactly one discovered file");
    assert_eq!(
        result[0].skipped.as_deref(),
        Some("hash-stamped generated file (pass --fix-generated to format)"),
        "a hash-stamped file must be skipped regardless of what the fmt cache holds for it"
    );
    assert!(!result[0].changed, "a skipped file must not be reported as changed");
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        stamped,
        "a skipped file must be left byte-for-byte unchanged on disk, not overwritten with a cached result"
    );
}
