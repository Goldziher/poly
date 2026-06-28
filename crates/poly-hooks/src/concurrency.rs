//! Concurrency sizing and `ARG_MAX` file batching for the rayon runner.
//!
//! `CONCURRENCY` mirrors prek: `PREK_NO_CONCURRENCY` forces serial,
//! `PREK_MAX_CONCURRENCY` caps the pool, otherwise the CPU count is used. The
//! `-j` request override (when present) wins over the environment.
//!
//! [`partition_files`] greedily splits a hook's matched files into batches that
//! each fit within the platform command-line limit, so a hook invoked over a
//! huge file set is split into several invocations the runner can `par_iter`.

use std::ffi::OsStr;
use std::path::Path;
use std::sync::LazyLock;

use crate::consts::env_vars::EnvVars;
use tracing::warn;

/// Resolve the effective concurrency from the no-concurrency flag, an optional
/// max-concurrency string, and the detected CPU count.
#[must_use]
pub fn resolve_concurrency(
    no_concurrency: bool,
    max_concurrency: Option<&str>,
    cpu: usize,
) -> usize {
    if no_concurrency {
        return 1;
    }
    if let Some(value) = max_concurrency {
        if let Ok(cap) = value.parse::<usize>() {
            return cap.max(1);
        }
        warn!(
            value = value,
            var = EnvVars::PREK_MAX_CONCURRENCY,
            "invalid max-concurrency value; falling back to CPU count"
        );
    }
    cpu.max(1)
}

/// The environment-derived default concurrency (computed once).
pub static CONCURRENCY: LazyLock<usize> = LazyLock::new(|| {
    let cpu = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    resolve_concurrency(
        EnvVars::is_set(EnvVars::PREK_NO_CONCURRENCY),
        EnvVars::var(EnvVars::PREK_MAX_CONCURRENCY).ok().as_deref(),
        cpu,
    )
});

/// The number of rayon worker threads to use for a run.
///
/// An explicit `-j` override (`Some(n)`, clamped to ≥ 1) wins; otherwise the
/// environment-derived [`CONCURRENCY`] applies.
#[must_use]
pub fn effective_concurrency(request_override: Option<usize>) -> usize {
    match request_override {
        Some(n) => n.max(1),
        None => *CONCURRENCY,
    }
}

// ── `ARG_MAX` batching ───────────────────────────────────────────────────────────

/// POSIX recommends leaving headroom so the child can set its own environment.
const ARG_HEADROOM: usize = 2048;
/// Conservative pointer size (64-bit) for argv/envp accounting.
const POINTER_SIZE: usize = 8;

fn arg_size(arg: &OsStr) -> usize {
    POINTER_SIZE + arg.len() + 1
}

#[cfg(unix)]
fn platform_max_cli_length() -> usize {
    // SAFETY: `sysconf` is always safe to call with a valid name constant; it
    // reads a system limit and has no preconditions or side effects.
    let arg_max = unsafe { libc::sysconf(libc::_SC_ARG_MAX) };
    let arg_max = if arg_max <= 0 {
        1 << 12
    } else {
        usize::try_from(arg_max).unwrap_or(1 << 20)
    };
    // SAFETY: see above — `sysconf(_SC_PAGE_SIZE)` is equally precondition-free.
    let page = unsafe { libc::sysconf(libc::_SC_PAGE_SIZE) };
    let page = if page < 4096 {
        4096
    } else {
        usize::try_from(page).unwrap_or(4096)
    };
    arg_max
        .saturating_sub(page)
        .saturating_sub(ARG_HEADROOM)
        .clamp(1 << 12, 1 << 20)
}

#[cfg(not(unix))]
fn platform_max_cli_length() -> usize {
    (1 << 15) - ARG_HEADROOM
}

/// Split `files` into batches whose argv sizes each fit within the platform
/// command-line limit, given `base_len` bytes already consumed by the program,
/// fixed args, and the environment.
///
/// Always returns at least one batch: an empty `files` slice yields a single
/// empty batch so the hook still runs once (preserving `always_run` / no-file
/// stage semantics).
#[must_use]
pub fn partition_files<'a>(files: &'a [&'a Path], base_len: usize) -> Vec<&'a [&'a Path]> {
    if files.is_empty() {
        return vec![&files[0..0]];
    }

    let max = platform_max_cli_length();
    let mut batches = Vec::new();
    let mut start = 0;
    let mut used = base_len;

    for (index, file) in files.iter().enumerate() {
        let size = arg_size(file.as_os_str());
        // Start a new batch when adding this file would overflow — but never
        // emit an empty batch (always include at least one file per batch).
        if index > start && used + size > max {
            batches.push(&files[start..index]);
            start = index;
            used = base_len;
        }
        used += size;
    }
    batches.push(&files[start..]);
    batches
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{partition_files, resolve_concurrency};

    #[test]
    fn no_concurrency_forces_serial() {
        assert_eq!(resolve_concurrency(true, Some("8"), 16), 1);
    }

    #[test]
    fn max_concurrency_caps_below_cpu() {
        assert_eq!(resolve_concurrency(false, Some("2"), 16), 2);
    }

    #[test]
    fn invalid_max_falls_back_to_cpu() {
        assert_eq!(resolve_concurrency(false, Some("nonsense"), 4), 4);
    }

    #[test]
    fn empty_files_yields_single_empty_batch() {
        let batches = partition_files(&[], 0);
        assert_eq!(batches.len(), 1);
        assert!(batches[0].is_empty());
    }

    #[test]
    fn small_set_is_a_single_batch() {
        let a = Path::new("a.rs");
        let b = Path::new("b.rs");
        let files = [a, b];
        let batches = partition_files(&files, 0);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].len(), 2);
    }

    #[test]
    fn oversized_base_splits_into_one_file_per_batch() {
        let a = Path::new("a.rs");
        let b = Path::new("b.rs");
        let files = [a, b];
        // A base length at the platform limit forces each file into its own batch.
        let batches = partition_files(&files, super::platform_max_cli_length());
        assert_eq!(batches.len(), 2);
    }
}
