---
priority: critical
---

# Module Size Cap

- Every file under `crates/**/src/**/*.rs` is capped at **1000 lines** by the `rust-max-lines`
  hook (see `poly.toml` → `[hooks.pre-commit.scripts.rust-max-lines]`). Files named `tests.rs`
  and files under a `tests/` directory are exempt.
- When a file approaches the cap, refactor by extracting helpers, types, or submodules — **do
  not raise the cap**.
- The cap reinforces the project's one-concern-per-file shape: a backend lives in
  `crates/poly-core/src/engines/<tool>.rs`, and when it outgrows the cap it is split per
  concern (e.g. `engines/<tool>/lint.rs`, `engines/<tool>/format.rs`, `engines/<tool>/config.rs`)
  rather than left as one oversized module. Pipeline stages (`discover.rs`, `cache.rs`,
  `runner.rs`, `report.rs`, …) each stay in their own file for the same reason.
</content>
