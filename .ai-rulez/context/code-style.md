---
priority: high
---

# Code Style

Project-specific conventions baked into context so they ship into every AI tool's config.

## Module layout

- **One concern per file.** A backend lives in `crates/polylint-core/src/engines/<tool>.rs`;
  pipeline stages live in their own files (`discover.rs`, `cache.rs`, `runner.rs`,
  `report.rs`, …). Match that shape when adding a new tool area.
- **1000-line cap** on every `*.rs` file, enforced by the `rust-max-lines` prek hook. Refactor
  by extracting helpers, types, or submodules — never by lifting the cap. When an
  `engines/<tool>.rs` grows past it, split per backend concern (e.g. `<tool>/lint.rs`,
  `<tool>/format.rs`, `<tool>/config.rs`).
- Per-backend tests live alongside the pipeline contract in `crates/polylint-core/tests/`.

## Performance

- **rayon `par_iter` over discovered files** is the parallelism unit — saturate available
  cores. Never spawn raw threads or `tokio::spawn` in the runner.
- **blake3 content-hash caching** skips unchanged work; the cache key folds in engine name +
  version + resolved config.
- Reuse tree-sitter parsers via a pool — never construct one per file.
- Avoid `.clone()` in inner loops; prefer `&str` / `&[u8]`. Defer ownership to the boundary.

## Dependency policy

- **Pure-Rust, no subprocess, no system dependency — ever.** Each wrapped tool is a crate dep.
- **Wrap first, vendor as fallback.** If a crate externalizes the API we need, wrap it. Only
  if it does not, **vendor** its source into `vendor/` and record the upstream commit +
  license in `vendor/ATTRIBUTIONS.md`. No version pinning, no git-rev deps.
- Empirically verify a tool's crate API by cloning it to `/tmp` before wrapping.
- `cargo deny` gates licenses (no GPL/AGPL); vendored sources must pass it.

## Opinionated defaults

- Respect each wrapped tool's own defaults as the base, then apply a thin override layer.
- **Line length 120** everywhere a tool exposes the setting.
- **Always format docstrings** (`docstring-code-format = true`, length 120).
- Purely stylistic rules: pick one modern convention or turn the rule off — never bikeshed.
- Layering order: tool default → opinionated override → user `polylint.toml`.

## Commits

- **Conventional Commit prefixes** (`feat:`, `fix:`, `perf:`, `chore:`, `refactor:`); enforced
  on `commit-msg` by the gitfluff hook. The body explains *why*, not *what*.
- **Commits are signed.**
</content>
