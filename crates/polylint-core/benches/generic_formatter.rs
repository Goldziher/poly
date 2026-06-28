//! Microbenchmarks for the tier-2 generic (tree-sitter) formatter — the
//! dominant fmt-wall-clock cost on Rust-heavy corpora (~476 files/s vs ~5–10k
//! for native backends).
//!
//! Three measurements isolate the stages so the flamegraph attribution can be
//! confirmed quantitatively:
//!
//! - `format_rust` — the full public `Engine::format` (parse + CST walk +
//!   reindent + serialize). This is the hot path.
//! - `parse_only_rust` — `tree-sitter` parse of the same source with a reused
//!   parser (mirrors the pooled parser). Subtract from `format_rust` to
//!   attribute the reindent+serialize cost.
//! - `normalize_whitespace` — the tier-2 fallback path (no parse), the floor
//!   cost for every non-brace-family / unparsable file.
//!
//! Run with: `cargo bench -p polylint-core --bench generic_formatter`.

use std::hint::black_box;
use std::path::PathBuf;
use std::sync::Arc;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use polylint_core::config::{EngineConfig, GlobalDefaults};
use polylint_core::defaults::normalize_whitespace;
use polylint_core::engine::{Engine, SourceFile};
use polylint_core::engines::treesitter::TreeSitterEngine;
use polylint_core::language::Language;
use tree_sitter_language_pack::get_parser;

/// A single, deterministic, *valid* Rust unit whose indentation is deliberately
/// flattened so the reindenter has real work to do per line (a realistic
/// "needs formatting" input rather than already-canonical source).
const RUST_UNIT: &str = r#"
pub struct Widget {
name: String,
count: usize,
tags: Vec<String>,
}

impl Widget {
pub fn new(name: &str) -> Self {
Self {
name: name.to_string(),
count: 0,
tags: Vec::new(),
}
}

pub fn tally(&mut self, items: &[Item]) -> usize {
for item in items {
if item.active {
self.count += item.weight;
for tag in &item.tags {
if !self.tags.contains(tag) {
self.tags.push(tag.clone());
}
}
}
}
self.count
}
}

fn helper(values: &[i64]) -> i64 {
let mut total = 0;
for v in values {
match v {
0 => continue,
n if *n < 0 => total -= n,
_ => total += v,
}
}
total
}
"#;

/// Build a deterministic Rust source of roughly `target_bytes` by repeating the
/// unit, renaming each copy so the parser sees distinct items (no dedup tricks).
fn sample_rust(target_bytes: usize) -> String {
    let mut out = String::with_capacity(target_bytes + RUST_UNIT.len());
    let mut i = 0usize;
    while out.len() < target_bytes {
        out.push_str(&RUST_UNIT.replace("Widget", &format!("Widget{i}")));
        i += 1;
    }
    out
}

fn engine_config() -> EngineConfig {
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 4,
        options: toml::Table::new(),
    }
}

fn source(content: &str) -> SourceFile {
    SourceFile {
        path: PathBuf::from("bench.rs"),
        language: Language::Other("rust".into()),
        content: Arc::from(content),
    }
}

fn bench_generic_formatter(c: &mut Criterion) {
    // ~16 KiB — a representative single source file on a Rust-heavy corpus.
    let content = sample_rust(16 * 1024);
    let cfg = engine_config();
    let engine = TreeSitterEngine;

    let mut group = c.benchmark_group("generic_formatter");
    group.throughput(Throughput::Bytes(content.len() as u64));

    // Full hot path: parse + CST walk + reindent + serialize.
    group.bench_function("format_rust", |b| {
        let src = source(&content);
        b.iter(|| {
            let out = engine.format(black_box(&src), black_box(&cfg)).unwrap();
            black_box(out);
        });
    });

    // Parse stage in isolation, parser reused across iterations (pooled-parser
    // behaviour). Difference vs `format_rust` ≈ reindent + serialize cost.
    group.bench_function("parse_only_rust", |b| {
        let mut parser = get_parser("rust").expect("rust grammar");
        b.iter(|| {
            let tree = parser.parse(black_box(content.as_str()));
            black_box(tree);
        });
    });

    // Tier-2 fallback path (no parse): trailing-whitespace trim + line-ending /
    // final-newline normalization. The floor cost for every file.
    let globals = GlobalDefaults::default();
    group.bench_function("normalize_whitespace", |b| {
        b.iter(|| {
            let out = normalize_whitespace(black_box(content.as_str()), black_box(&globals));
            black_box(out);
        });
    });

    group.finish();
}

criterion_group!(benches, bench_generic_formatter);
criterion_main!(benches);
