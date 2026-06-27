---
priority: high
---

# Crate Layout

polylint is a Cargo **workspace** that ships two self-contained binaries driven by one
config: `polylint` (lint) and `polyfmt` (format). Everything runs **in-process, pure Rust** —
no subprocess, no system dependency, ever. A tool is consumed as a crate dependency: from
crates.io when published, otherwise from a **pinned git `rev`** of its upstream repo (e.g.
oxc's `oxc_formatter`/`oxc_linter`, ruff's internals). We do **not** vendor and we do **not**
publish our own crates to crates.io — binaries are distributed as prebuilt release artifacts
plus an installer (see release-versioning).

## Workspace root

- `Cargo.toml` — `[workspace]` (resolver 3, edition 2024). Members: `crates/polylint-core`,
  `crates/polylint`, `crates/polyfmt`, `crates/poly-cli`. Shared deps live under
  `[workspace.dependencies]`; git deps are pinned to a `rev` (monorepo crates share one `rev`).
- `deny.toml` — `cargo deny` license / source allow-list (no GPL/AGPL), applied across the full
  dependency tree including git deps and their transitive dependencies.

## `crates/polylint-core/` — the engine library (path dep; not published)

`src/`:

- `lib.rs` — public re-exports.
- `engine.rs` — the **`Engine` trait** contract plus `SourceFile`, `Capabilities`,
  `Severity`, `Span`, `Edit`, `Diagnostic`, `FormatOutput`. This is the keystone abstraction.
- `registry.rs` — `Language -> &dyn Engine` resolution. A static `match` (alef-style): native
  crate backend if one is registered for the language, else the tree-sitter generic backend.
- `config.rs` — TOML schema (`polylint.toml`, comment-preserving via `toml_edit`; YAML
  auto-detected via saphyr but TOML wins), normalization, and per-engine config slices.
- `defaults.rs` — the thin opinionated override layer (line length 120, always format
  docstrings, line-ending/final-newline). Layering is **tool default → opinionated override →
  user `polylint.toml`**.
- `cache.rs` — blake3 content-hash cache over `(file bytes + engine name + engine version +
  resolved engine config)`. Atomic sibling-tmp-then-rename + `fd-lock`; platform cache dir via
  `dirs`; `--no-cache` bypasses.
- `discover.rs` — file walk via the `ignore` crate (respects `.gitignore`).
- `runner.rs` — the pipeline: discover → cache → engine → report, parallelized with **rayon
  `par_iter` over files**.
- `report.rs` — human (colored, `annotate-snippets`) and JSON output.
- `language.rs` — `Language` enum + detection (delegates to `tree-sitter-language-pack`).
- `engines/` — one file per backend, `engines/<tool>.rs`:
  - native crate backends: `ruff.rs`, `oxc.rs`, `taplo.rs`, `rumdl.rs`, `sqruff.rs`,
    `malva.rs`, `markup_fmt.rs`, `graphql.rs`, `nixfmt.rs`, `typos.rs`, `yaml.rs`.
  - `whitespace.rs` / `treesitter.rs` — the **tier-2 generic formatter** built on
    `tree-sitter-language-pack`: CST-driven structural reindent + whitespace normalization,
    the catch-all for every language without a native backend.

## `crates/polylint/` and `crates/polyfmt/` — the binaries (thin)

Each is a thin clap CLI over `polylint-core`. `polylint [PATHS]… --fix --format human|json
--config <p> --no-cache -j <N> --no-color`; `polyfmt [PATHS]… --check …`. Ship
`.pre-commit-hooks.yaml` so a consuming repo replaces its whole hook list with two hooks.

## The `Engine` trait contract (`engine.rs`)

Every backend — native crate or generic tier — implements the same trait:

```rust
pub trait Engine: Send + Sync {
    fn name(&self) -> &'static str;
    fn languages(&self) -> &[Language];
    fn capabilities(&self) -> Capabilities;   // lint / format / fix
    fn version(&self) -> &str;                 // folded into the cache key
    fn lint(&self, src: &SourceFile, cfg: &EngineConfig) -> Result<Vec<Diagnostic>>;
    fn format(&self, src: &SourceFile, cfg: &EngineConfig) -> Result<FormatOutput>;
}
pub enum FormatOutput { Unchanged, Formatted(String) }
```

Contract rules: `Engine` is `Send + Sync` (it runs inside a rayon `par_iter`); `version()`
must change whenever output could change, because it is part of the cache key; `format`
returns `Unchanged` rather than echoing input so the runner can skip writes; `lint`/`format`
default to no-ops for engines that lack a capability (declare honestly via `capabilities()`).

## Two-tier coverage

1. **Native Rust crate backend** where one exists — highest-fidelity output, registered for
   its specific languages in `registry.rs`.
2. **Tree-sitter generic tier** (`whitespace.rs` / `treesitter.rs`) — the catch-all for
   *everything else* (shell, Go, Java, Kotlin, Ruby, PHP, Elixir, C/C++, Dockerfile, protobuf,
   Rust, and the long tail of 300+ grammars). Best-effort structural reindent, pure Rust,
   grammars fetched on demand → still zero system deps. This is the coverage mechanism, not a
   fallback to avoid; native ports can later upgrade individual languages from tier-2 to
   tier-1 fidelity.

## Tests

- `crates/polylint-core/tests/pipeline.rs` — end-to-end pipeline contract.
- Per-backend `insta` fixtures: a known-bad file (expected `Diagnostic`s) and a
  known-unformatted file (exact formatted output).
