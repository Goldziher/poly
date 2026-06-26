# 0009 — Parallelism: rayon Over Files, Saturate All Cores

- Status: Accepted
- Date: 2026-06-26

## Context

The workloads are large repos with thousands of files. Lint/format of one file is
independent of another, and the `Engine` trait is `Send + Sync` (ADR 0004) precisely so
work can fan out. Modern dev and CI machines have many cores (the dev box is 14 cores /
48 GB) that a serial runner would leave idle.

## Decision

The runner parallelizes **with `rayon`, across files**, and is built to **saturate all
available cores**. The pipeline — discover (via the `ignore` crate, respecting
`.gitignore`) → cache lookup (ADR 0008) → engine invocation → collect report — runs each
file as a rayon work item. A `-j <N>` flag caps concurrency when a user wants to; the
default is "use the whole machine".

## Consequences

Positive:

- Near-linear speedup on multi-core machines; large-repo runs and CI get dramatically
  faster.
- File-level parallelism is the natural grain — no shared mutable state between files, so
  little contention.
- Pairs well with the content-hash cache: hits are resolved in parallel and cheaply.

Negative / risks:

- Backends must be genuinely thread-safe in practice; a wrapped/vendored tool with hidden
  global state could misbehave under parallelism and needs isolation or serialization.
- Saturating cores raises peak memory (many parsers live at once); on constrained
  environments `-j` may be needed to avoid OOM.
- Diagnostic output ordering is nondeterministic across threads, so the reporter must sort
  results (e.g. by path) for stable, reproducible output.

## Alternatives considered

- **Serial processing:** rejected — wastes available hardware; unacceptably slow on the
  target corpus.
- **Async / tokio:** rejected — the work is CPU-bound, not IO-bound; rayon's data
  parallelism is the right model and matches the alef template.
- **Per-engine internal threading:** rejected as the primary mechanism — file-level
  parallelism is simpler, more uniform, and composes across all backends.
