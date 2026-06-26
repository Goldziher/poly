# 0008 — Caching: blake3 Content-Hash, Atomic Writes, Supersedes Tool Caches

- Status: Accepted
- Date: 2026-06-26

## Context

Lint/format runs are dominated by re-processing unchanged files, especially in pre-commit
and CI. Several wrapped tools have their own internal caches, but those are per-tool,
keyed differently, and invisible to us — we cannot rely on them for a uniform, correct
incremental experience across every backend.

## Decision

A single **content-hash result cache**, owned by polylint-core:

- **Key = blake3 over `(engine name, engine version, resolved engine config, file
  bytes)`.** All four components matter: changing the file, upgrading the engine
  (`version()` from the `Engine` trait, ADR 0004), or changing resolved options
  (ADR 0006/0007) all produce a fresh key and a cache miss.
- **Value = the engine's output** (diagnostics for lint, formatted bytes / `Unchanged`
  for format).
- **Atomic writes:** write to a sibling temp file then rename, guarded by `fd-lock` —
  the cache/atomic-write pattern copied from `ts-pack-core/src/download.rs`.
- Stored in the platform cache dir (via `dirs`); `--no-cache` bypasses entirely.
- **Our cache supersedes each tool's internal cache:** we disable/ignore upstream caches
  where possible and treat ours as the single source of incremental truth.

## Consequences

Positive:

- Correct, uniform incremental behavior across every backend with one invalidation model.
- blake3 is fast enough that hashing is cheap relative to lint/format work.
- Atomic writes + `fd-lock` make the cache safe under the parallel runner (ADR 0009) and
  concurrent invocations.
- Folding engine `version` into the key means upgrades never serve stale results.

Negative / risks:

- Correctness hinges on the key capturing _every_ input that affects output; a hidden
  input (e.g. an env var, a sibling file a tool reads, ambient locale) not in the key
  causes stale hits. Each backend adapter must declare its real inputs.
- Disabling upstream tool caches may lose some intra-tool optimizations; we accept this for
  a single coherent cache.
- A cache directory to manage, garbage-collect, and occasionally invalidate wholesale.

## Alternatives considered

- **mtime/size-based caching:** rejected — fragile across checkouts, clones, and CI where
  timestamps reset; content hashing is robust.
- **Rely on each tool's own cache:** rejected — fragmented, inconsistent keys, and no
  uniform `--no-cache` or invalidation story.
- **No cache:** rejected — pre-commit and CI on large repos would be needlessly slow.
