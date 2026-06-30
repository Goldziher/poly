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
//!
//! Run with: `cargo bench -p polylint-core --bench cache_key`.

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use poly_cache::{Namespace, ResultCache};

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
    // File sizes spanning the corpus: tiny (1 KiB), typical (8 KiB), large (64 KiB).
    let sizes = [1024usize, 8 * 1024, 64 * 1024];
    let args = ResultCache::serialize_args(&toml::Table::new());

    let mut group = c.benchmark_group("cache_key");
    for &size in &sizes {
        let content = content_of(size);
        group.throughput(Throughput::Bytes(content.len() as u64));

        // The dominant cost: blake3 over the whole file.
        group.bench_with_input(
            BenchmarkId::new("single_file_digest", size),
            &content,
            |b, c| {
                b.iter(|| black_box(ResultCache::single_file_digest(black_box(c))));
            },
        );

        // The full per-engine-per-file key cost the runner pays: digest + key.
        group.bench_with_input(
            BenchmarkId::new("digest_plus_key", size),
            &content,
            |b, c| {
                b.iter(|| {
                    let digest = ResultCache::single_file_digest(black_box(c));
                    let key = ResultCache::key_with_args(
                        Namespace::Lint,
                        "treesitter",
                        "5",
                        black_box(&args),
                        &digest,
                    );
                    black_box(key);
                });
            },
        );
    }
    group.finish();

    // Key-preamble hashing alone (digest precomputed): isolates the fixed
    // per-key cost from the content-proportional digest cost above.
    let digest = ResultCache::single_file_digest(&content_of(8 * 1024));
    c.bench_function("cache_key/key_with_args_only", |b| {
        b.iter(|| {
            let key = ResultCache::key_with_args(
                Namespace::Lint,
                "treesitter",
                "5",
                black_box(&args),
                black_box(&digest),
            );
            black_box(key);
        });
    });
}

/// Cache `get` on a miss vs a hit. The miss path is the one a whole-repo run
/// hits tens of thousands of times: it must be served from the in-memory
/// presence index with no syscall (it used to cost a failing `open()`), so a
/// regression that reintroduces a per-miss syscall shows up here as the miss
/// approaching the hit's cost.
fn bench_cache_get(c: &mut Criterion) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("cache");
    let args = ResultCache::serialize_args(&toml::Table::new());

    // Populate one entry, then reopen so the presence-index snapshot includes it
    // (mirrors a warm run that scans the on-disk tree at startup).
    {
        let cache = ResultCache::open(root.clone(), true).expect("open cache");
        let digest = ResultCache::single_file_digest("cached payload");
        let present = ResultCache::key_with_args(Namespace::Lint, "eng", "1", &args, &digest);
        cache.put(Namespace::Lint, &present, b"payload").expect("put");
    }
    let cache = ResultCache::open(root, true).expect("reopen cache");
    let present = ResultCache::key_with_args(
        Namespace::Lint,
        "eng",
        "1",
        &args,
        &ResultCache::single_file_digest("cached payload"),
    );
    let absent = ResultCache::key_with_args(
        Namespace::Lint,
        "eng",
        "1",
        &args,
        &ResultCache::single_file_digest("never stored"),
    );

    let mut group = c.benchmark_group("cache_get");
    group.bench_function("miss_via_presence_index", |b| {
        b.iter(|| black_box(cache.get(Namespace::Lint, black_box(&absent))));
    });
    group.bench_function("hit_reads_file", |b| {
        b.iter(|| black_box(cache.get(Namespace::Lint, black_box(&present))));
    });
    group.finish();
}

#[path = "support/profiler.rs"]
mod profiler;

criterion_group! {
    name = benches;
    config = Criterion::default().with_profiler(profiler::FlamegraphProfiler::new(997));
    targets = bench_cache_key, bench_cache_get
}
criterion_main!(benches);
