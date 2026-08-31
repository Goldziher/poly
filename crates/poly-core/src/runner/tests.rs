//! Tests for the pipeline runner.
//!
//! Split out because `runner.rs` sits at the repository's 1000-line module cap;
//! a path named `tests.rs` is exempt from it.

use super::*;

#[test]
fn cache_write_policy_skips_cheap_results() {
    assert!(!should_cache_result(
        MIN_CACHE_DURATION - std::time::Duration::from_nanos(1)
    ));
    assert!(should_cache_result(MIN_CACHE_DURATION));
}

#[test]
fn disabled_cache_skips_digest_work() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cache = ResultCache::open(tmp.path().join("cache"), false).expect("open disabled cache");
    let digest_computed = std::cell::Cell::new(false);

    let digest = digest_if_enabled(&cache, || {
        digest_computed.set(true);
        ResultCache::single_file_digest("content")
    });

    assert!(digest.is_none());
    assert!(!digest_computed.get(), "disabled caching must not hash file contents");
}

/// A pass that is already at its fixed point runs exactly once — no wasted
/// confirmation pass, and the content is returned unchanged.
#[test]
fn format_to_fixed_point_stable_input_runs_once() {
    let calls = std::cell::Cell::new(0);
    let result = format_to_fixed_point(Arc::from("stable"), |content, _record| {
        calls.set(calls.get() + 1);
        Ok(Arc::clone(content))
    })
    .unwrap();
    assert_eq!(&*result.0, "stable");
    assert!(result.1, "an unchanged input is at its fixed point");
    assert_eq!(calls.get(), 1, "an already-stable input needs a single pass");
}

/// A non-idempotent pass (strips one trailing '!' per run) converges within
/// the bound, and the driver stops as soon as a pass makes no change — so the
/// result is a genuine fixed point, mirroring the `fmt --fix` then `--check`
/// invariant.
#[test]
fn format_to_fixed_point_converges_on_non_idempotent_pass() {
    let calls = std::cell::Cell::new(0);
    let result = format_to_fixed_point(Arc::from("a!!!"), |content, _record| {
        calls.set(calls.get() + 1);
        let stripped = content.strip_suffix('!').unwrap_or(content);
        Ok(Arc::from(stripped))
    })
    .unwrap();
    assert_eq!(&*result.0, "a", "trailing markers fully removed");
    assert!(result.1, "and it settled inside the bound");
    // "a!!!"->"a!!"->"a!"->"a" is 3 changing passes plus 1 no-op that proves
    // stability = 4 calls. ~keep
    assert_eq!(calls.get(), 4);
}

/// Only the first pass records debug so per-engine timing counts are not
/// inflated by the convergence retries.
#[test]
fn format_to_fixed_point_records_debug_on_first_pass_only() {
    let recorded: std::cell::RefCell<Vec<bool>> = std::cell::RefCell::new(Vec::new());
    format_to_fixed_point(Arc::from("a!!"), |content, record| {
        recorded.borrow_mut().push(record);
        Ok(Arc::from(content.strip_suffix('!').unwrap_or(content)))
    })
    .unwrap();
    assert_eq!(
        recorded.into_inner(),
        vec![true, false, false],
        "debug is recorded on the first pass, never on a retry"
    );
}

/// A backend that never stabilizes is bounded to `MAX_FORMAT_PASSES` runs
/// rather than looping forever — and **says** it did not settle.
///
/// Reporting that is the whole point. The hardening harness found real HTML
/// files where `poly fmt --fix` produced different bytes on seven successive
/// runs while reporting success each time; a following `poly fmt --check`
/// reports drift forever, and nothing in the output explained why.
#[test]
fn format_to_fixed_point_reports_a_never_stable_pass() {
    let calls = std::cell::Cell::new(0);
    let (content, settled) = format_to_fixed_point(Arc::from("x"), |content, _record| {
        calls.set(calls.get() + 1);
        Ok(Arc::from(format!("{content}y")))
    })
    .unwrap();
    assert_eq!(calls.get(), MAX_FORMAT_PASSES, "oscillation is capped, not infinite");
    assert_eq!(&*content, "xyyyyy", "returns the last bounded pass output");
    assert!(!settled, "and reports that the file is still changing");
}

/// Recurse `depth` frames, each pinning ~8 KiB of stack, returning the
/// accumulated depth. `black_box` keeps the per-frame buffer from being
/// optimised away, so the stack actually grows.
fn recurse_pinning_stack(depth: usize) -> usize {
    let mut frame = [0u8; 8 * 1024];
    frame[0] = (depth & 0xff) as u8;
    std::hint::black_box(&frame);
    if depth == 0 {
        frame[0] as usize
    } else {
        recurse_pinning_stack(depth - 1).wrapping_add(1)
    }
}

/// A worker thread sized at [`WORKER_STACK_SIZE`] must accommodate recursion
/// far deeper than the 2 MiB default rayon stack — the regression that made
/// per-file engines abort the whole run on nested real-world files
/// (spikard corpus). ~640 frames × 8 KiB ≈ 5 MiB of pinned stack overflows
/// the old 2 MiB default but fits comfortably in 16 MiB.
#[test]
fn worker_stack_accommodates_deep_recursion() {
    const FRAMES: usize = 640;
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .stack_size(WORKER_STACK_SIZE)
        .build()
        .expect("build local pool");
    let result = pool.install(|| recurse_pinning_stack(FRAMES));
    assert_eq!(result, FRAMES);
}
