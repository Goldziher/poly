//! Microbenchmarks for the result-cache key path — flagged wave-3 as "a wash at
//! corpus scale": content-hashing ~28k files costs about as much as the cheap
//! lint work it saves. These benches quantify the per-file cache overhead so the
//! follow-up can decide whether to mmap, skip hashing small files, or drop the
//! lint cache entirely.
//!
//! Stages mirror what `runner::lint_content` does per file:
//! - `single_file_digest`  — blake3 over the file bytes (the dominant cost).
//! - `key_with_args`        — blake3 over the small key preamble (precomputed digest).
//! - `digest_plus_key`      — the realistic per-engine-per-file cache-key cost.
//! - `cache_get`            — on-disk lookup latency for a hit versus a miss.
//!
//! Run with: `cargo bench -p poly-core --bench cache_key`.

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use poly_cache::{Namespace, ResultCache};
use rayon::prelude::*;

const INDEX_OPEN_ENTRY_COUNT: usize = 10_000;
const PARALLEL_MISS_BATCH_SIZE: usize = 1_024;

/// Deterministic source-like content of a given byte length (repeated line so
/// the digest exercises a realistic file size without RNG nondeterminism).
fn content_of(bytes: usize) -> String {
    let line = "let value = compute(some_argument, another_argument); // a typical line\n";
    let mut s = String::with_capacity(bytes + line.len());
    while s.len() < bytes {
        s.push_str(line);
    }
    s
}

fn bench_cache_key(c: &mut Criterion) {
    let sizes = [1024usize, 8 * 1024, 64 * 1024];
    let args = ResultCache::serialize_args(&toml::Table::new());

    let mut group = c.benchmark_group("cache_key");
    for &size in &sizes {
        let content = content_of(size);
        group.throughput(Throughput::Bytes(content.len() as u64));

        group.bench_with_input(BenchmarkId::new("single_file_digest", size), &content, |b, c| {
            b.iter(|| black_box(ResultCache::single_file_digest(black_box(c))));
        });

        group.bench_with_input(BenchmarkId::new("digest_plus_key", size), &content, |b, c| {
            b.iter(|| {
                let digest = ResultCache::single_file_digest(black_box(c));
                let key = ResultCache::key_with_args(Namespace::Lint, "treesitter", "5", black_box(&args), &digest);
                black_box(key);
            });
        });
    }
    group.finish();

    let digest = ResultCache::single_file_digest(&content_of(8 * 1024));
    c.bench_function("cache_key/key_with_args_only", |b| {
        b.iter(|| {
            let key =
                ResultCache::key_with_args(Namespace::Lint, "treesitter", "5", black_box(&args), black_box(&digest));
            black_box(key);
        });
    });
}

fn bench_cache_get(c: &mut Criterion) {
    let temp_dir = tempfile::tempdir().expect("create cache benchmark directory");
    let cache_root = temp_dir.path().to_path_buf();
    let cache = ResultCache::open(cache_root.clone(), true).expect("open benchmark cache");
    let args = ResultCache::serialize_args(&toml::Table::new());
    let hit_digest = ResultCache::single_file_digest("cached content");
    let hit_key = ResultCache::key_with_args(Namespace::Lint, "treesitter", "5", &args, &hit_digest);
    let miss_digest = ResultCache::single_file_digest("uncached content");
    let miss_key = ResultCache::key_with_args(Namespace::Lint, "treesitter", "5", &args, &miss_digest);
    let later_digest = ResultCache::single_file_digest("written after open");
    let later_key = ResultCache::key_with_args(Namespace::Lint, "treesitter", "5", &args, &later_digest);
    cache
        .put(Namespace::Lint, &hit_key, b"[]")
        .expect("seed cache benchmark entry");
    drop(cache);
    let cache = ResultCache::open(cache_root, true).expect("reopen populated benchmark cache");

    let mut group = c.benchmark_group("cache_get");
    group.bench_function("hit", |b| {
        b.iter(|| black_box(cache.get(Namespace::Lint, black_box(&hit_key))));
    });
    group.bench_function("miss", |b| {
        b.iter(|| black_box(cache.get(Namespace::Lint, black_box(&miss_key))));
    });
    cache
        .put(Namespace::Lint, &later_key, b"[]")
        .expect("write an entry after reopening the benchmark cache");
    group.bench_function("miss_after_put", |b| {
        b.iter(|| black_box(cache.get(Namespace::Lint, black_box(&miss_key))));
    });
    group.bench_function("parallel_miss_after_put_1024", |b| {
        b.iter(|| {
            black_box(
                (0..PARALLEL_MISS_BATCH_SIZE)
                    .into_par_iter()
                    .filter_map(|_| cache.get(Namespace::Lint, black_box(&miss_key)))
                    .count(),
            )
        });
    });
    group.finish();
}

fn bench_cache_open(c: &mut Criterion) {
    let temp_dir = tempfile::tempdir().expect("create cache-open benchmark directory");
    let cache_root = temp_dir.path().to_path_buf();
    drop(ResultCache::open(cache_root.clone(), true).expect("initialize benchmark cache"));
    let lint_dir = cache_root.join("results/lint");
    for index in 0..INDEX_OPEN_ENTRY_COUNT {
        std::fs::write(lint_dir.join(format!("{index:064x}")), b"[]").expect("seed cache index entry");
    }
    c.bench_function("cache_open/10000_entries", |b| {
        b.iter(|| black_box(ResultCache::open(cache_root.clone(), true).expect("open populated cache")));
    });
}

criterion_group!(benches, bench_cache_key, bench_cache_get, bench_cache_open);
criterion_main!(benches);
