//! A criterion [`Profiler`] that captures a CPU flamegraph for any bench run
//! under `--profile-time`.
//!
//! Criterion calls [`start_profiling`]/[`stop_profiling`] only when a bench is
//! invoked with `--profile-time=<secs>`; normal `cargo bench` runs are
//! unaffected. While profiling, [`pprof`] samples the call stack at a fixed
//! frequency and, on stop, writes a `flamegraph.svg` into criterion's
//! per-benchmark output directory (`target/criterion/<group>/<bench>/profile/`).
//!
//! This wraps [`pprof::ProfilerGuard`] directly rather than enabling pprof's own
//! `criterion` feature, so it stays decoupled from whichever criterion version
//! that feature happens to pin.
//!
//! [`start_profiling`]: Profiler::start_profiling
//! [`stop_profiling`]: Profiler::stop_profiling

use std::ffi::c_int;
use std::fs::File;
use std::path::Path;

use criterion::profiler::Profiler;
use pprof::ProfilerGuard;

/// Sampling-based flamegraph profiler for criterion `--profile-time` runs.
pub struct FlamegraphProfiler<'a> {
    /// Sampling frequency in Hz.
    frequency: c_int,
    /// The active pprof guard while a bench is being profiled.
    guard: Option<ProfilerGuard<'a>>,
}

impl FlamegraphProfiler<'_> {
    /// Build a profiler sampling at `frequency` Hz. 997 (a prime near 1 kHz) is
    /// a good default — high enough for detail, prime to avoid aliasing with
    /// periodic work.
    pub fn new(frequency: c_int) -> Self {
        Self {
            frequency,
            guard: None,
        }
    }
}

impl Profiler for FlamegraphProfiler<'_> {
    fn start_profiling(&mut self, _benchmark_id: &str, _benchmark_dir: &Path) {
        match ProfilerGuard::new(self.frequency) {
            Ok(guard) => self.guard = Some(guard),
            Err(error) => eprintln!("flamegraph profiler: failed to start pprof: {error}"),
        }
    }

    fn stop_profiling(&mut self, _benchmark_id: &str, benchmark_dir: &Path) {
        let Some(guard) = self.guard.take() else {
            return;
        };
        if let Err(error) = std::fs::create_dir_all(benchmark_dir) {
            eprintln!(
                "flamegraph profiler: create {}: {error}",
                benchmark_dir.display()
            );
            return;
        }
        let svg_path = benchmark_dir.join("flamegraph.svg");
        let report = match guard.report().build() {
            Ok(report) => report,
            Err(error) => {
                eprintln!("flamegraph profiler: build report: {error}");
                return;
            }
        };
        match File::create(&svg_path) {
            Ok(file) => {
                if let Err(error) = report.flamegraph(file) {
                    eprintln!("flamegraph profiler: write {}: {error}", svg_path.display());
                } else {
                    eprintln!("flamegraph profiler: wrote {}", svg_path.display());
                }
            }
            Err(error) => eprintln!(
                "flamegraph profiler: create {}: {error}",
                svg_path.display()
            ),
        }
    }
}
