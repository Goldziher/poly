---
priority: high
---

# Performance Discipline

polylint processes whole repositories on every lint/format run, so the hot path is
**per-file parallelism**. The runner (`crates/polylint-core/src/runner.rs`) discovers files,
checks the cache, dispatches to an engine, and collects reports. Apply these patterns by
default; deviate only with measurement.

- **rayon `par_iter` over the discovered file set is the parallelism unit.** Saturate
  available cores. Never spawn raw `std::thread` and never use `tokio::spawn` in the runner —
  the pipeline is CPU-bound, synchronous, and rayon-driven end to end.
- **Reuse tree-sitter parsers via a pool.** Parsers and compiled queries are expensive to
  build; pull them from the pool in the generic tier — never construct one per file.
- **blake3 content-hash cache skips unchanged work.** The key folds in file bytes + engine
  name + engine `version()` + resolved engine config; a cache hit must short-circuit before
  the engine runs. Keep `version()` honest so output changes invalidate the cache.
- **Avoid `.clone()` in inner loops.** Inside the `par_iter` body (and inside an `Engine::lint`
  / `Engine::format` call), pass `&str` / `&[u8]` rather than cloning `String` / `Vec<u8>`.
  Defer any required ownership to the boundary.
- Prefer borrowing over allocation throughout the per-file path; an `Engine` runs once per
  file per run, so allocations there multiply by the corpus size.
- Before optimizing further, measure against a real corpus (the xberg-io repos are the dry-run
  test corpus) and capture the wall-clock delta.
</content>
