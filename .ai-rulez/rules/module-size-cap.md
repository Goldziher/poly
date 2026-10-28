---
type: Decision
title: Module Size Cap
x-ai-rulez:
  kind: rule
  id: module-size-cap
  metadata:
    priority: critical
---

# Module Size Cap

- **Every `*.rs` file in the repository** is capped at **1000 lines** by the `rust-max-lines`
  hook (`poly.toml` → `[hooks.pre-commit.scripts.rust-max-lines]`, `files = "**/*.rs"`,
  `args = ["--max=1000"]`). The scope is not limited to `crates/**/src/**`.
- Exemptions, as the hook actually implements them
  (`scripts/hooks/rust-max-lines.sh`): a path under a `tests/` directory at any depth, and a
  file named `tests.rs`. `poly.toml` additionally excludes the generated
  `crates/poly-hooks/src/identify/tags.rs`. Nothing else is exempt.
- When a file approaches the cap, refactor by extracting helpers, types, or submodules — **do
  not raise the cap**.
- The cap reinforces the project's one-concern-per-file shape: a backend starts as
  `crates/poly-core/src/engines/<tool>.rs`, and when it outgrows the cap it becomes a directory
  split per concern (`engines/<tool>/lint.rs`, `format.rs`, `config.rs`) rather than one
  oversized module — as `engines/oxc/`, `engines/quality/`, `engines/treesitter/`,
  `engines/astgrep/`, `engines/mago/`, `engines/hcl/`, `engines/native_tool/` and
  `engines/catalog_tool/` already have (`engines/ruff.rs` keeps its submodules in `engines/ruff/`).
  Pipeline stages follow the same shape: `discover.rs` is a single file, `resolve.rs` carries a
  `resolve/` submodule directory (`canon.rs`, `scan.rs`), `runner.rs` carries `runner/` (`plan.rs`,
  `edits.rs`, `skips.rs`, `types.rs`, `batch.rs`, …), and reporting is the `report/` directory. The result cache is not a `poly-core`
  module at all — it is the separate `poly-cache` crate.
