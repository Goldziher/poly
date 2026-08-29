---
priority: high
---

# Performance Discipline

poly processes whole repositories on every lint/format run, so the hot path is
**per-file parallelism**. The runner (`crates/poly-core/src/runner.rs`, with its `runner/`
submodules) discovers files, checks the cache, dispatches to an engine, and collects reports.
Apply these patterns by default; deviate only with measurement.

- **rayon `par_iter` over the discovered file set is the parallelism unit.** Saturate
  available cores. Never spawn raw `std::thread` and never use `tokio::spawn` in the runner —
  the pipeline is CPU-bound, synchronous, and rayon-driven end to end. The runner builds its
  own rayon pool with a **16 MiB worker stack** (`WORKER_STACK_SIZE` in `runner.rs`) because
  tree-sitter recursion overflows rayon's 2 MiB default; keep that pool, don't fall back to
  the global one.
- **Reuse tree-sitter parsers via a pool.** Parsers and compiled queries are expensive to
  build; pull them from the pool — never construct one per file. The pools are `thread_local!`
  maps keyed by grammar name: `engines/treesitter/mod.rs` and `engines/treesitter/indent.rs`
  (the latter caches the `QueryCursor` and compiled `Query` too), `engines/uncomment.rs`, and
  `engines/quality/mod.rs`, whose pool is shared by every structural sub-rule so a file is
  parsed once per lint pass rather than once per rule.
  **Exception — do not add one for ast-grep.** `ast_grep_core::AstGrep::try_new` owns the parse
  and `ast-grep-core` already pools the underlying `tree_sitter::Parser` per thread per language
  behind it (its `PARSER_CACHE` thread-local). A poly-side pool there was tried and reverted as
  measurably useless; see the comment in `engines/astgrep/mod.rs`. Skipping the parse entirely
  when no rule applies is the win that did pay off.
- **blake3 content-hash cache skips unchanged work.** The key
  (`ResultCache::key_with_args`, `crates/poly-cache/src/lib.rs`) folds in, in order: cache
  format version, **build identity** of the running binary, storage namespace, engine/hook `id`,
  engine `version()`, the TOML-serialized args (`EngineConfig.options` for engines), and the
  input digest — `blake3` over the file's path **and** bytes. A cache hit must short-circuit
  before the engine runs. Keep `version()` honest so output changes invalidate the cache.
  Serialize the args table **once per engine** with `ResultCache::serialize_args` and borrow the
  result into every per-file `key_with_args` call (see `runner/plan.rs`) — never re-serialize
  inside the loop.
- **Avoid `.clone()` in inner loops.** Inside the `par_iter` body (and inside an `Engine::lint`
  / `Engine::format` call), pass `&str` / `&[u8]` rather than cloning `String` / `Vec<u8>`.
  Defer any required ownership to the boundary. Anything derivable once per language or per
  engine belongs in the pre-computed plan (`runner/plan.rs`), not in the per-file body.
- Prefer borrowing over allocation throughout the per-file path; an `Engine` runs once per
  file per run, so allocations there multiply by the corpus size.
- **Measure before and after.** `cargo bench -p poly-core` runs the criterion hot-path benches
  (`crates/poly-core/benches/`: `generic_formatter`, `cache_key`, `runner_e2e`); their inputs are
  synthetic and deterministic, and `benches/README.md` carries the committed baseline to beat.
  For whole-repository wall-clock numbers, run against a real corpus (the xberg-io repos are the
  dry-run corpus) and capture the delta.
