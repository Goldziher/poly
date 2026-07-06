# 0008 — Caching: blake3 Content-Hash, Two-Tier, CACHE_FORMAT_VERSION

- Status: Accepted
- Date: 2026-06-26
- Updated: 2026-06-28 (`poly-cache` crate introduces two-tier cache, hook-specific
  soundness model, CACHE_FORMAT_VERSION, `poly cache` CLI)
- Updated: 2026-07-05 (whole-workspace hooks: staged-content digest + default-on `cargo`
  group caching; see ADR 0019)
- Updated: 2026-07 (v0.9.0): the cache moved out of the repo into the per-user OS cache dir
  (`~/.cache/poly/<repo-key>`); the in-repo `.polylint/` directory is retired.

## Context

Lint/format runs are dominated by re-processing unchanged files, especially in pre-commit
and CI. As `poly` grows into an umbrella family (ADR 0011) that includes git-hooks and
commit-message linting, caching must cover all three workloads — engines (lint/format) and
hooks (which may mutate the tree). Each tier has different cache-correctness constraints.

## Decision

The `poly-cache` crate provides a **two-tier cache** in the per-user OS cache directory —
`~/.cache/poly/<repo-key>` (Linux / `$XDG_CACHE_HOME`), `~/Library/Caches/poly/…` (macOS),
`%LOCALAPPDATA%\poly\…` (Windows). `POLY_CACHE_HOME` overrides the base; `[cache] dir` pins
an explicit root:

> **Update (2026-07, v0.9.0):** the cache (result cache and the ADR 0019 staged snapshot)
> moved out of the repo's in-tree `.polylint/` directory into the per-user OS cache dir,
> keyed per repository, so nothing cache-related is written under version control anymore. A
> legacy in-repo `.polylint/` is auto-removed on the next run.

**Tier 1: Result cache** (`results/` subdirectory, namespaced)

- **Namespaces:** `Namespace::{Lint, Fmt, Hook}` — one result key type per workload.
- **Key = blake3 over `(namespace, engine name, engine version, resolved config, file
  bytes)` for engines**, or **`(namespace, hook name, version, declared inputs)`** for
  hooks. All components affect output and must be in the key.
- **For lint operations, the key additionally includes the file path** to capture
  path-dependent diagnostics (e.g. ruff's INP001 import-not-in-init-file rule). For
  formatting, the key does not include the path since formatted output is path-independent.
- **The effective `[defaults]` globals** (line_length, line_ending, final_newline,
  trim_trailing_whitespace) **and indent_width are folded into the key**, so overrides to
  these settings invalidate cached results.
- **Value = the engine's or hook's output:** diagnostics for lint, formatted bytes /
  `Unchanged` for format, hook result + stdout/stderr for hooks.
- **CACHE_FORMAT_VERSION:** included in the cache-dir structure to invalidate the entire
  cache if the schema changes (e.g. adding new fields to result tuples).
- **Atomic writes:** write to a sibling temp file then rename, guarded by `fd-lock`.
- **Our cache supersedes each tool's internal cache:** engines disable/ignore upstream
  caches; we're the single source of incremental truth.

**Tier 2: Opt-in compiler cache** (sccache)

- For hooks marked `compiler = true` (e.g. Rust's `cargo clippy`), delegate to sccache
  if available. This tier is entirely optional and off by default.

**Hook-specific caching soundness model:**

- Builtins (e.g. `typos`, `trailing-whitespace`) cache by default when they only examine
  matched files (safe).
- Inline commands (user scripts) never cache unless explicitly `cache.inputs = [...]`
  with mode `safe` (only reads these files) or `aggressive` (caches despite risk).
- Tree-mutating hooks (those that write to disk) **never cache** — each run must execute.
- **Whole-workspace hooks (ADR 0019) key on staged content.** A hook marked `workspace =
  true` (e.g. the `cargo` group, `pyrefly`) analyses the whole project, so its declared
  inputs are resolved from the whole tree, but under staged isolation the digested **bytes
  come from the staged snapshot**, not the worktree — otherwise reverting an unstaged edit
  would be a false hit. The `cargo` group is result-cached **by default** on the Rust
  source/manifest set (`**/*.rs`, `Cargo.toml`, `Cargo.lock`, `deny.toml`, toolchain), so a
  commit touching no Rust skips `clippy`/`sort`/`machete`/`deny`; opt out with `cargo =
  { cache = false }`.

**Bypass:** `--no-cache` disables caching for the run. `poly cache gc` / `poly cache
clean` / `poly cache stats` / `poly cache size` manage the cache directory.

## Consequences

Positive:

- Correct, uniform incremental behavior across lint/format/hooks with one invalidation
  model per namespace.
- blake3 is fast enough that hashing is cheap relative to lint/format/hook work.
- Atomic writes + `fd-lock` make the cache safe under the parallel runner (ADR 0009) and
  concurrent invocations.
- Folding engine/hook `version` into the key means upgrades never serve stale results.
- `CACHE_FORMAT_VERSION` allows non-breaking additions to the cache schema.
- Hook-specific soundness rules (safe/aggressive, never cache tree-mutators) prevent
  silent correctness bugs.
- The `poly cache` CLI gives users visibility and control: stats, size estimation,
  garbage collection, and wholesale cleanup.

Negative / risks:

- Correctness hinges on the key capturing _every_ input that affects output; a hidden
  input (e.g. an env var, a sibling file a tool reads) not in the key causes stale hits.
  Each backend and hook adapter must declare its real inputs — discipline required.
- Disabling upstream tool caches may lose some intra-tool optimizations; we accept this for
  a single coherent cache.
- A cache directory to manage; developers must occasionally gc or clean it.
- For hooks, the soundness model requires discipline: inline commands must correctly
  declare their inputs to use `safe`/`aggressive` modes. Incorrect declarations can cause
  stale hook results.

## Alternatives considered

- **mtime/size-based caching:** rejected — fragile across checkouts, clones, and CI where
  timestamps reset; content hashing is robust.
- **Rely on each tool's own cache:** rejected — fragmented, inconsistent keys, and no
  uniform `--no-cache` or invalidation story.
- **No cache:** rejected — pre-commit and CI on large repos would be needlessly slow.
- **Single global version (not namespaced):** rejected — lint, format, and hooks have
  different versioning and cache-correctness models; namespacing decouples them.
