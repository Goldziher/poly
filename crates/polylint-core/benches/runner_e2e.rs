//! End-to-end runner bench: discover → route → format over a small synthetic
//! Rust corpus written to a temp dir. Exercises the rayon `par_iter` pipeline,
//! the registry, and the tier-2 formatter together, so the follow-up can see the
//! whole-pipeline wall-clock alongside the isolated-stage benches.
//!
//! Inputs are synthetic and written to a `tempfile::TempDir` (no dependency on
//! ../xberg-io or any external corpus — runs anywhere). The cache is disabled
//! (`no_cache: true`) so every run does real formatting work and the numbers are
//! comparable across runs.
//!
//! Run with: `cargo bench -p polylint-core --bench runner_e2e`.

use std::hint::black_box;
use std::path::PathBuf;

use criterion::{Criterion, criterion_group, criterion_main};
use polylint_core::config::Config;
use polylint_core::runner::{RunOptions, format};
use tempfile::TempDir;

/// One deterministic, flattened-indent Rust file; the `{i}` suffix keeps each
/// generated file distinct.
fn rust_file(i: usize) -> String {
    format!(
        "pub struct Item{i} {{\n\
         id: usize,\n\
         label: String,\n\
         }}\n\
         \n\
         impl Item{i} {{\n\
         pub fn new(id: usize) -> Self {{\n\
         Self {{\n\
         id,\n\
         label: String::new(),\n\
         }}\n\
         }}\n\
         pub fn bump(&mut self, by: usize) -> usize {{\n\
         for _ in 0..by {{\n\
         self.id += 1;\n\
         }}\n\
         self.id\n\
         }}\n\
         }}\n"
    )
}

/// Write `count` synthetic `.rs` files into a fresh temp dir and return it.
fn corpus(count: usize) -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for i in 0..count {
        let path = dir.path().join(format!("file_{i}.rs"));
        std::fs::write(&path, rust_file(i)).expect("write fixture");
    }
    dir
}

fn bench_runner(c: &mut Criterion) {
    const FILE_COUNT: usize = 64;
    let dir = corpus(FILE_COUNT);
    let paths = vec![PathBuf::from(dir.path())];
    let config = Config::default();
    // Disable the cache so each iteration does the full format work (otherwise
    // the second iteration would be an all-hit no-op and measure cache reads).
    let opts = RunOptions {
        no_cache: true,
        jobs: None,
    };

    let mut group = c.benchmark_group("runner_e2e");
    // Heavier than the micro-benches; keep the sample count modest.
    group.sample_size(20);
    group.bench_function("format_64_rust_files_dry_run", |b| {
        b.iter(|| {
            let results = format(
                black_box(&paths),
                black_box(&config),
                black_box(&opts),
                false,
                false,
            )
            .expect("format run");
            black_box(results);
        });
    });
    group.finish();
}

#[path = "support/profiler.rs"]
mod profiler;

criterion_group! {
    name = benches;
    config = Criterion::default().with_profiler(profiler::FlamegraphProfiler::new(997));
    targets = bench_runner
}
criterion_main!(benches);
