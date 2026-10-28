---
type: Concept
title: Crate Layout
x-ai-rulez:
  kind: context
  id: crate-layout
  metadata:
    priority: high
---

# Crate Layout

poly is a Cargo **workspace** that ships a single self-contained binary, `poly`, driven by one
config. Its subcommands cover the whole surface: `poly lint`, `poly fmt`, `poly hooks`, `poly
commit`, `poly rules`, `poly cache`, `poly config`, `poly migrate`, `poly mcp`, and `poly
doctor`. Everything runs **in-process, pure Rust** — no subprocess, no system dependency by
default (three scoped exceptions: **native-toolchain backends**, the opt-in **catalog-tool**
tier, and the `poly hooks` engine — see Coverage tiers below).

A tool is consumed as a crate dependency, **from crates.io whenever a usable version is
published**. That includes ruff, which moved off its git pin on 2026-08-29: `ruff_linter =
"=0.17.3"` and `ruff_db` / `ruff_formatter` / `ruff_python_ast` / `ruff_python_formatter` /
`ruff_python_semantic` / `ruff_text_size` = `"=0.0.20"`. The `=` is deliberate — these are unsemvered internals, and an
exact pin preserves the property the git `rev` had. The only remaining **pinned git `rev`** deps
are **oxc**, **biome**, and **rubyfmt**, each for a reason recorded in ADR 0003's 2026-08-29
amendment: four of oxc's crates are `publish = false` and a partial migration is a hard compile
error (a registry `SourceType` will not typecheck against the git `oxc_formatter`); biome's
published crates ship 2024 code under today's version numbers; the crates.io `rubyfmt` is a
`0.0.0-wip` name reservation. A git pin records upstream's publishing posture at a moment in
time, not a permanent property — re-check the pins when touching dependencies. We do **not**
vendor, with one exception: the archived upstream `dockerfile-parser` is maintained in-tree as the
`crates/dockerfile-parser` workspace crate (ADR 0003 amendment, `ATTRIBUTIONS.md`). We do **not**
publish our own crates to crates.io — the binary is distributed as prebuilt release artifacts plus
an installer (see release-versioning).

## Workspace root

- `Cargo.toml` — `[workspace]` (resolver 3, edition 2024, `rust-version = "1.99"`). Twelve
  members: `poly-core` (the engine library), `poly-cli` (builds the `poly` binary),
  `poly-config`, `poly-cache`, `poly-catalog`, `poly-hooks`, `poly-workspace`, `poly-mcp`,
  `poly-buildinfo`, and three that do **not** carry the prefix — `gitfluff` (the commit-message
  linter behind `poly commit`), `dockerfile-parser` (the vendored Dockerfile parser behind the
  `dockerfile` engine) and `conformance` (a dev-only differential formatter-conformance harness
  the shipped binary never depends on). Shared deps live under `[workspace.dependencies]`; git deps are pinned to a `rev`
  (crates from one monorepo share one `rev`).
- `deny.toml` — `cargo deny` license / source allow-list (no GPL/AGPL), applied across the full
  dependency tree including git deps and their transitive dependencies.
- `crates/poly-workspace/` — the whole-project (workspace) lint orchestration shared by
  `poly-cli` and `poly-mcp`: lowers `[hooks]` config into the `poly-hooks` model, reduces it to
  the whole-project tool set (`cargo clippy`/`cargo-sort`/`cargo-machete`/`cargo-deny`), and runs
  it against the live worktree. Public API is narrow —
  `run_workspace_lint(&PolyConfig, &WorkspaceLintOptions) -> WorkspaceLintOutcome` plus
  `render_workspace_outcome` — so both consumers get identical behavior without a dependency
  cycle: the crate never loads config itself, the caller injects an already-resolved
  `poly_config::PolyConfig` (the CLI keeps its git-remote `extends` resolver, the MCP server
  keeps its network-free one).
- `.ai-rulez/` holds contributor rules, context and agents. The curated product source is
  `plugin/.ai-rulez/`, including its `skills/`, `commands/` and `config.plugins.toml`.
  Generated `CLAUDE.md` / `AGENTS.md`, repository marketplace indexes, and manifests under
  `plugin/` must never be hand-edited. Root indexes resolve the public member bundle and
  keep maintainer content out of installs.
  `scripts/release-bump.sh <version>` bumps the workspace and plugin versions together,
  regenerates the plugin outputs, and asserts the generated manifests match before finishing.
