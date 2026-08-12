# 0008 — Caching: blake3 Content-Hash, Two-Tier, CACHE_FORMAT_VERSION

- Status: Accepted
- Date: 2026-06-26
- Updated: 2026-06-28 (`poly-cache` crate introduces two-tier cache, hook-specific
  soundness model, CACHE_FORMAT_VERSION, `poly cache` CLI)
- Updated: 2026-07-05 (whole-workspace hooks: staged-content digest + default-on `cargo`
  group caching; see ADR 0019)
- Updated: 2026-07 (v0.9.0): the cache moved out of the repo into the per-user OS cache dir
  (`~/.cache/poly/<repo-key>`); the in-repo `.polylint/` directory is retired.
- Updated: 2026-08-12: the running binary's **build identity** joins the key preamble, so
  invalidation no longer depends on anyone remembering to bump a string; the key also folds
  `CACHE_FORMAT_VERSION` (now `4`), and the run path sweeps stranded entries automatically.

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
- **CACHE_FORMAT_VERSION:** written to the `VERSION` sentinel *and* folded into the key, so
  a schema change both makes existing entries unreachable and reclaims their bytes.
- **Atomic writes:** write to a sibling temp file then rename, guarded by `fd-lock`.
- **Our cache supersedes each tool's internal cache:** engines disable/ignore upstream
  caches; we're the single source of incremental truth.

**Key preamble: a layered identity (2026-08-12).**

| Layer            | Source                             | Invalidates when                   |
| ---------------- | ---------------------------------- | ---------------------------------- |
| `format_version` | `CACHE_FORMAT_VERSION`             | what is *stored* changes shape     |
| `build_identity` | `poly_buildinfo::cache_identity()` | the poly binary itself changes     |
| `id` + `version` | `Engine::name` / `Engine::version` | a wrapped upstream crate is bumped |
| `args`           | resolved engine config             | the user reconfigures the engine   |
| `input_digest`   | file bytes (+ path for lint)       | the file changes                   |

Only `build_identity` is **automatic**, and that is the point: `Engine::version()` is a
hand-written string, and the `version_audit` test can only check that it tracks the *wrapped
crate's* version. A change to poly's **own** logic — a heuristic, a default, a config
mapping — moved no key component at all, so every previously cached file kept being served
pre-change results. Two builds that both call themselves `0.19.7` (a dev build and the
release, or two dev builds) produced identical keys and read each other's entries.

The identity is scoped by build channel, which is the whole trade-off:

- **Release** (release profile *and* a build id equal to `v<VERSION>`) → `release/<version>`.
  Nothing machine-local participates, so `v0.19.7` on CI and on a laptop share entries. A
  released version is immutable, so the version is a sufficient proof of sameness.
- **Development / unknown** → channel, version, `git describe` id, profile **and a
  stat-cheap fingerprint of the executable** (device, inode, size, mtime). `git describe`
  separates commits but is blind to uncommitted work — the case an iterating developer hits
  constantly — so the executable fingerprint is what separates two builds of one commit.
  A development build therefore re-runs its work after every rebuild, which is the correct
  price for never trusting a verdict produced by code that no longer exists.

**Eviction.** Every invalidation strands a generation of entries that nothing will ever key
again. `gc` existed but only ran when a human typed `poly cache gc`, so the cache grew
without bound; with a per-rebuild identity it would grow faster. The run-path open now
sweeps automatically, at most once per 24 h per repo cache (recorded by a `LAST_SWEEP`
marker), evicting entries older than 14 days and then trimming oldest-first to a 1 GiB
ceiling. A failed sweep is logged and ignored — losing disk space must not fail a lint run.

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
- Folding the **build identity** into the key means no *build* ever serves another build's
  results — the invalidation that used to depend on a remembered `version()` bump is now
  correct by construction, including for changes to poly's own logic.
- Stranded generations are reclaimed automatically instead of accumulating until someone
  runs `poly cache gc`.
- `CACHE_FORMAT_VERSION` allows non-breaking additions to the cache schema.
- Hook-specific soundness rules (safe/aggressive, never cache tree-mutators) prevent
  silent correctness bugs.
- The `poly cache` CLI gives users visibility and control: stats, size estimation,
  garbage collection, and wholesale cleanup.

Negative / risks:

- Correctness hinges on the key capturing *every* input that affects output; a hidden
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
- **Hashing the executable to identify a development build:** rejected on measured cost —
  blake3 over the 83 MiB release binary is ~45 ms (~55 ms with the read) and ~500 ms over
  the 353 MiB debug binary, paid on *every* invocation including the three-file pre-commit
  gate. It is the only scheme under which two byte-identical dev builds keep sharing a
  cache; that benefit does not justify the fixed cost.
- **`git describe --dirty`:** rejected — the build script only re-runs when `.git` moves, so
  the dirty bit would itself be stale, and it is one bit: every distinct uncommitted state
  would still collide.
- **Hashing the workspace sources at build time:** rejected — it would force
  `poly-buildinfo`, and therefore every crate above it, to rebuild on every source edit,
  wrecking the development loop it is meant to protect.
- **A per-build timestamp or nonce in the identity:** rejected — it would also invalidate
  across two identical *release* builds, destroying cross-machine and CI cache reuse.
