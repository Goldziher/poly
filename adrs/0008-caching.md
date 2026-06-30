# 0008 — Caching: blake3 Content-Hash, Two-Tier, CACHE_FORMAT_VERSION

- Status: Accepted
- Date: 2026-06-26
- Updated: 2026-06-28 (`poly-cache` crate introduces two-tier cache, hook-specific
  soundness model, CACHE_FORMAT_VERSION, `poly cache` CLI)

## Context

Lint/format runs are dominated by re-processing unchanged files, especially in pre-commit
and CI. As `poly` grows into an umbrella family (ADR 0011) that includes git-hooks and
commit-message linting, caching must cover all three workloads — engines (lint/format) and
hooks (which may mutate the tree). Each tier has different cache-correctness constraints.

## Decision

The `poly-cache` crate provides a **two-tier cache** under `.polylint/cache/`:

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
