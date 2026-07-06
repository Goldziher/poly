---
priority: high
---

# Code Style

Project-specific conventions baked into context so they ship into every AI tool's config.

## Module layout

- **One concern per file.** A backend lives in `crates/poly-core/src/engines/<tool>.rs`;
  pipeline stages live in their own files (`discover.rs`, `cache.rs`, `runner.rs`,
  `report.rs`, …). Match that shape when adding a new tool area.
- **1000-line cap** on every `*.rs` file, enforced by the `rust-max-lines` hook in `poly.toml`. Refactor
  by extracting helpers, types, or submodules — never by lifting the cap. When an
  `engines/<tool>.rs` grows past it, split per backend concern (e.g. `<tool>/lint.rs`,
  `<tool>/format.rs`, `<tool>/config.rs`).
- Per-backend tests live alongside the pipeline contract in `crates/poly-core/tests/`.

## Performance

- **rayon `par_iter` over discovered files** is the parallelism unit — saturate available
  cores. Never spawn raw threads or `tokio::spawn` in the runner.
- **blake3 content-hash caching** skips unchanged work; the cache key folds in engine name +
  version + resolved config.
- Reuse tree-sitter parsers via a pool — never construct one per file.
- Avoid `.clone()` in inner loops; prefer `&str` / `&[u8]`. Defer ownership to the boundary.

## Dependency policy

- **Pure-Rust, in-process, no subprocess, no system dependency — by default.** Every wrapped
  tool is compiled in as a crate dependency; engines never shell out. **One scoped, opt-in
  exception:** *native-toolchain backends* (see crate-layout) may invoke a language's canonical
  first-party CLI — `gofmt`, `rustfmt`, `zig fmt`, … — when it is present on the host. They are
  **off by default**, and when the tool is absent the language falls through to the tree-sitter
  tier, so the zero-dependency guarantee still holds for everyone who hasn't opted in. (`poly
  hooks`/polyhooks is a separate, pre-existing exception, since running foreign hooks inherently
  shells out.)
- **Prefer crates.io; use a pinned git dependency when the library we need isn't published**
  (or only stale/yanked versions are). Several upstream tools ship a usable library only in
  their monorepo — oxc's `oxc_formatter` (oxfmt) and `oxc_linter` (oxlint), and ruff's
  internals — so depend on the GitHub repo pinned to a specific `rev` (commit) for
  reproducibility. When several crates come from one monorepo (e.g. all of oxc), pin them to the
  **same `rev`** so their internal versions stay consistent.
- **Do not vendor.** A pinned git dep tracks upstream without a forked copy to maintain. (There
  is no `vendor/` directory.)
- We do **not** publish our own crates to crates.io (see release-versioning), so git deps are
  fine — crates.io only forbids them when *publishing*, which we don't do.
- Empirically verify a tool's library API by cloning it to `/tmp` at the exact pinned `rev`
  before wiring it.
- Pin the git `rev` and commit `Cargo.lock` for reproducible builds.
- `cargo deny` gates licenses (no GPL/AGPL) across the full dependency tree, git deps included.

## Opinionated defaults

- Respect each wrapped tool's own defaults as the base, then apply a thin override layer.
- **Line length 120** everywhere a tool exposes the setting.
- **Always format docstrings** (`docstring-code-format = true`, length 120).
- Purely stylistic rules: pick one modern convention or turn the rule off — never bikeshed.
- Layering order: tool default → opinionated override → user `poly.toml`.

## Commits

- **Conventional Commit prefixes** (`feat:`, `fix:`, `perf:`, `chore:`, `refactor:`); enforced
  on `commit-msg` by the gitfluff hook. The body explains *why*, not *what*.
- **Commits are signed.**
</content>
