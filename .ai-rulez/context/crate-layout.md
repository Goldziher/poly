---
priority: high
---

# Crate Layout

poly is a Cargo **workspace** that ships a single self-contained binary, `poly`, driven by one
config. Its subcommands cover the whole surface: `poly lint`, `poly fmt`, `poly hooks`, `poly
commit`, `poly rules`, `poly cache`, `poly mcp`, and `poly migrate`. Everything runs
**in-process, pure Rust** — no subprocess, no system dependency by default (two scoped
exceptions: opt-in **native-toolchain backends** and the `poly hooks` engine — see Coverage
tiers below). A tool is consumed as a crate dependency: from crates.io when published, otherwise
from a **pinned git `rev`** of its upstream repo (e.g. oxc's `oxc_formatter`/`oxc_linter`, ruff's
internals). We do **not** vendor and we do **not** publish our own crates to crates.io — the
binary is distributed as prebuilt release artifacts plus an installer (see release-versioning).

## Workspace root

- `Cargo.toml` — `[workspace]` (resolver 3, edition 2024). Every workspace crate uses the
  `poly-` prefix: `crates/poly-core` (the engine library) and the `poly-` CLI crate that builds
  the `poly` binary, alongside the other members. Shared deps live under
  `[workspace.dependencies]`; git deps are pinned to a `rev` (monorepo crates share one `rev`).
- `deny.toml` — `cargo deny` license / source allow-list (no GPL/AGPL), applied across the full
  dependency tree including git deps and their transitive dependencies.

## `crates/poly-core/` — the engine library (lib `poly_core`; path dep; not published)

`src/`:

- `lib.rs` — public re-exports.
- `engine.rs` — the **`Engine` trait** contract plus `SourceFile`, `Capabilities`,
  `Severity`, `Span`, `Edit`, `Diagnostic`, `FormatOutput`. This is the keystone abstraction.
- `registry.rs` — `Language -> &dyn Engine` resolution. A static `match` (alef-style): native
  crate backend if one is registered for the language, else the tree-sitter generic backend.
- `config.rs` — TOML schema (`poly.toml`, comment-preserving via `toml_edit`; YAML
  auto-detected via saphyr but TOML wins), normalization, and per-engine config slices.
  `poly.local.toml` layers local overrides on top.
- `defaults.rs` — the thin opinionated override layer (line length 120, always format
  docstrings, line-ending/final-newline). Layering is **tool default → opinionated override →
  user `poly.toml`**.
- `cache.rs` — blake3 content-hash cache over `(file bytes + engine name + engine version +
  resolved engine config)`. Atomic sibling-tmp-then-rename + `fd-lock`; the cache lives in the
  per-user OS cache dir (`~/.cache/poly/<repo-key>`, `~/Library/Caches/poly/…`,
  `%LOCALAPPDATA%\poly\…`) via `dirs`, overridable with `POLY_CACHE_HOME` or pinned via
  `[cache] dir`; `--no-cache` bypasses.
- `discover.rs` — file walk via the `ignore` crate (respects `.gitignore`).
- `runner.rs` — the pipeline: discover → cache → engine → report, parallelized with **rayon
  `par_iter` over files**.
- `report.rs` — human (colored, `annotate-snippets`) and JSON output.
- `language.rs` — `Language` enum + detection (delegates to `tree-sitter-language-pack`).
- `engines/` — one file per backend, `engines/<tool>.rs`:
  - native crate backends: `ruff.rs`, `oxc.rs`, `taplo.rs`, `rumdl.rs`, `sqruff.rs`,
    `malva.rs`, `markup_fmt.rs`, `graphql.rs`, `nixfmt.rs`, `typos.rs`, `yaml.rs`.
  - `uncomment.rs` — the **opt-in cross-cutting comment-removal lint backend** wrapping the
    `uncomment` crate: reports each removable comment as a warning with a delete-edit, gated on
    `[lint.uncomment] enabled = true`. Cross-cutting like `typos` (`languages() == &[]`).
  - `whitespace.rs` / `treesitter.rs` — the **tier-2 generic formatter** built on
    `tree-sitter-language-pack`: CST-driven structural reindent + whitespace normalization,
    the catch-all for every language without a native backend.
  - `native_tool.rs` — the **opt-in native-toolchain backend** (table-driven): wraps a
    language's canonical first-party CLI (`gofmt`, `rustfmt`, `zig fmt`, …) as a subprocess
    when present and enabled. One file, one table — not one file per tool.

## The `poly` binary (thin CLI)

The `poly-` CLI crate is a thin clap wrapper over `poly-core`. Lint and format are subcommands:
`poly lint [PATHS]… --fix --format human|json --config <p> --no-cache -j <N> --no-color`; `poly
fmt [PATHS]… --check …`. A consuming repo collapses its hook sprawl onto poly's own `poly hooks`
runner via `poly.toml [hooks]` (ADR 0012) — poly no longer ships a `.pre-commit-hooks.yaml`, so
there is no external pre-commit-framework dependency in between.

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

## Coverage tiers

Three backend mechanisms, in resolution order per language:

1. **Native Rust crate backend (tier-1)** where one exists — highest-fidelity, fully
   in-process, zero deps; registered for its specific languages in `registry.rs`.
2. **Tree-sitter generic tier (tier-2)** (`whitespace.rs` / `treesitter.rs`) — the catch-all
   for *everything else* (shell, Go, Java, Kotlin, Ruby, PHP, Elixir, C/C++, Dockerfile,
   protobuf, Rust, and the long tail of 300+ grammars). Best-effort structural reindent, pure
   Rust, grammars fetched on demand → still zero system deps. This is the coverage mechanism,
   not a fallback to avoid; native ports can later upgrade individual languages from tier-2 to
   tier-1 fidelity.
3. **Native-toolchain backend** (`native_tool.rs`) — for languages whose *canonical*
   formatter/linter is a first-party CLI with no usable Rust library: Go's `gofmt`, Rust's
   `rustfmt`, Zig's `zig fmt`, and the like. This is the **single, scoped exception** to the
   no-subprocess rule (the `poly hooks` engine is the other, separate one). It exists because no
   pure-Rust crate can match these tools, and reimplementing them is a disproportionate
   maintenance sink. Strict discipline keeps the exception honest:

   - **`rustfmt` and `gofmt` are default-on when present; everything else is opt-in, off by
     default.** The two canonical formatters with no viable Rust library run automatically when
     found on PATH (ADR 0014 amendment, 2026-06-28), matching what `cargo fmt`/`gofmt` users
     already expect; `zig fmt` and every native *lint* tool stay opt-in via config
     (`[fmt.<lang>.<tool>] enabled = true`). Output then depends on the host tool's presence and
     version, which is at odds with reproducibility — so CI must pin the toolchain. Either way,
     when the tool is absent the language falls through to tier-2, so the zero-dependency promise
     is intact for anyone without the toolchain installed.
   - **Capability-gated, graceful degradation.** Probe for the tool once (cached); declare the
     `format`/`lint` capability only when it is found *and* enabled; otherwise the language
     falls through to tier-2. A missing toolchain is never an error — just lower fidelity.
   - **Per-file, stdin→stdout only.** Wrap only tools that process a single file over
     stdin/stdout (`gofmt`, `rustfmt`, `zig fmt`). Project-wide tools that must compile a
     package — `clippy`, `go vet`, `mix format` — do **not** fit the rayon per-file unit or the
     content-hash cache and are explicitly out of scope for this tier.
   - **Honest cache key + least privilege.** Fold the tool's resolved `--version` into
     `version()` so a toolchain upgrade invalidates the cache. Invoke with a fixed argv, no
     shell, content fed on stdin — never pass file contents through a shell.

## Tests

- `crates/poly-core/tests/pipeline.rs` — end-to-end pipeline contract.
- Per-backend `insta` fixtures: a known-bad file (expected `Diagnostic`s) and a
  known-unformatted file (exact formatted output).
