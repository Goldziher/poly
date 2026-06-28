# polylint-core hot-path benchmarks

Criterion microbenchmarks plus a profiling recipe for poly's per-file hot path.
This round is **measurement + setup only** — no engine/runner/cache logic was
changed. The numbers below are the baseline the follow-up optimization task
should beat.

## Running

```sh
# all three benches
cargo bench -p polylint-core

# one bench
cargo bench -p polylint-core --bench generic_formatter
cargo bench -p polylint-core --bench cache_key
cargo bench -p polylint-core --bench runner_e2e

# quick pass (less wall-clock; what the baseline below was captured with)
cargo bench -p polylint-core -- --warm-up-time 1 --measurement-time 3
```

Inputs are synthetic and deterministic (no dependency on `../xberg-io` or any
external corpus) so the benches run anywhere. `runner_e2e` writes a synthetic
Rust corpus into a `tempfile::TempDir` and runs with the cache disabled.

## What each bench covers

| Bench | Function | Hot-path stage |
|-------|----------|----------------|
| `generic_formatter/format_rust` | `TreeSitterEngine::format`, ~16 KiB Rust | parse + walk + reindent + serialize |
| `generic_formatter/parse_only_rust` | `Parser::parse` (reused parser) | CST parse stage in isolation |
| `generic_formatter/normalize_whitespace` | `defaults::normalize_whitespace` | tier-2 fallback floor (no parse) |
| `cache_key/single_file_digest/{1k,8k,64k}` | `ResultCache::single_file_digest` | blake3 hash (lint-cache cost) |
| `cache_key/digest_plus_key/{1k,8k,64k}` | digest + `key_with_args` | per-file cache-key cost |
| `cache_key/key_with_args_only` | `key_with_args` (digest precomputed) | fixed per-key preamble hash |
| `runner_e2e/format_64_rust_files_dry_run` | `runner::format`, 64 files, `no_cache` | discover → rayon → tier-2 |

## Baseline (Apple Silicon, release profile, `--measurement-time 3`)

Generic formatter on a 16 KiB Rust file:

| Bench | Median time | Throughput |
|-------|-------------|------------|
| `format_rust` (full) | **2.67 ms** | ~6.0 MiB/s |
| `parse_only_rust` | **1.43 ms** | ~11.2 MiB/s |
| `normalize_whitespace` | ~80 µs | ~200 MiB/s |

→ **Parse ≈ 54 % of the full format time; CST walk + reindent + serialize ≈ 46 %.**
The whitespace-only fallback is ~33× cheaper than the brace-family reindent path,
confirming the tree-sitter parse + reindent is what makes Rust-heavy corpora the
fmt bottleneck (the wave-3 ~476 files/s figure). At ~6 MiB/s single-threaded,
16 KiB/file ≈ 375 files/s/core — consistent with the corpus observation.

Cache key path:

| Bench | 1 KiB | 8 KiB | 64 KiB |
|-------|-------|-------|--------|
| `single_file_digest` | ~1.20 µs | ~4.9 µs | ~32 µs |
| `digest_plus_key` | ~1.51 µs | ~4.7 µs | ~31 µs |
| `key_with_args_only` | — | — | ~191 ns (size-independent) |

→ blake3 runs at ~0.8 GiB/s on 1 KiB files, rising to ~2.0 GiB/s at 64 KiB
(per-call setup amortizes). The fixed key preamble is ~191 ns. For a typical
8 KiB source the per-engine cache-key cost is **~4.7 µs**; against the cheap
tier-2 _lint_ (a single trailing-whitespace line scan, sub-µs/KiB) this is the
wave-3 "cache is a wash" result quantified — the hash often costs more than the
lint it memoizes. (Note: for _fmt_ the cache still pays off, since formatting a
file is ~2.7 ms ≫ the ~5 µs hash.)

End-to-end runner (64 synthetic Rust files, dry-run, cache disabled): **~1.4–2.7 ms**
total — i.e. the per-file tier-2 cost amortized across all cores via rayon.

## Profiling

samply works without sudo on cargo-compiled (locally-signed) binaries; it
**cannot** profile system binaries (`/bin/echo`, system python). Profile the
already-built bench binary in criterion's `--profile-time` loop:

```sh
cargo bench -p polylint-core --bench generic_formatter --no-run   # build it
BIN=$(ls -t target/release/deps/generic_formatter-* | grep -v '\.d$' | head -1)
samply record --save-only -o /tmp/poly_fmt_profile.json.gz -- \
    "$BIN" --bench --profile-time 8 format_rust
# then: samply load /tmp/poly_fmt_profile.json.gz   (opens Firefox Profiler UI)
```

`cargo flamegraph` is also installed but on macOS needs dtrace/sudo — prefer
samply. Note: the release profile (`lto = "thin"`, `codegen-units = 1`, no
debuginfo) strips most Rust symbols, so the raw profile shows addresses rather
than names for inlined Rust code; the captured baseline profile had **~16 % of
self-time concentrated in a 4-address cluster inside the tree-sitter parse
module**, matching the `parse_only` ≈ 54 % split above. For symbol-level Rust
attribution, add `debug = 1` to `[profile.bench]` before profiling.

## Ranked hotspots + optimization hypotheses (drives the follow-up)

1. **Tree-sitter CST parse — ~54 % of fmt time (top hotspot).**
   `Parser::parse` dominates `format_rust`. The parser is already pooled per
   thread (`thread_local! PARSERS`), so the cost is the parse automaton itself,
   not parser construction.
   - _Hypothesis A:_ skip the parse entirely when the file is already correctly
     indented — a cheap pre-pass (or hashing the would-be output) avoids the
     2.6 ms round-trip on no-op files, which dominate steady-state repos.
   - _Hypothesis B:_ the grammar is dynamically loaded; verify the language-pack
     download path isn't re-loading the grammar `.dylib` per file (it should be
     cached alongside the pooled parser).

2. **Reindent tree-walk + string building — ~46 % of fmt time.**
   `collect_cst` walks the whole CST building `Vec<Delimiter>` and
   `Vec<(usize,usize)>` (`protected`), then `reindent` allocates a fresh output
   `String` and, per non-empty line, calls `line.trim()` and pushes the `unit`
   string `level` times.
   - _Hypothesis A:_ `reindent` does `delimiters.iter().filter(...).collect()`
     into a fresh `Vec<&Delimiter>` **per line** (O(lines × delimiters)). Replace
     with a single forward cursor over the sorted `delimiters` slice — the
     delimiters are already byte-sorted, so a sweep is O(delimiters + lines).
   - _Hypothesis B:_ `CstFacts::is_interior` / `case_adjustment` are linear scans
     over `protected` / case ranges **per line**. With sorted ranges, binary
     search (or a merge sweep alongside the line walk) drops these to O(log n).
   - _Hypothesis C:_ push the indent unit once as a precomputed `level`-repeated
     string (or `out.extend(std::iter::repeat)`), and reserve `out` capacity from
     a depth estimate to cut reallocations.

3. **Lint result-cache hashing — a wash at corpus scale.**
   For lint, `single_file_digest` (~4.7 µs for 8 KiB) is comparable to or larger
   than the tier-2 lint it memoizes (a single sub-µs/KiB line scan).
   - _Hypothesis A:_ skip the lint cache for files below ~N KiB (the hash costs
     more than the recompute); keep it for expensive native linters (ruff/oxc).
   - _Hypothesis B:_ gate caching per engine by declared lint cost rather than
     applying it uniformly — the fmt cache clearly pays (2.7 ms ≫ 5 µs), the
     cheap-lint cache does not.
   - _Hypothesis C:_ `single_file_digest` routes through `file_set_digest`, which
     hashes the file bytes, then `.to_hex().to_string()` allocates a 64-char
     string, which is re-hashed (over ~66 bytes) plus another `.to_hex()`
     allocation for the digest, and `key_with_args` allocates a third hex string.
     The second hash is negligible (the first pass over file bytes dominates),
     but a single-file fast path that folds the raw 32-byte hashes through the
     key without the intermediate hex `String` allocations removes per-file heap
     traffic from the hot loop.
