# 0012 — Native Hook Runner Replaces the prek Bridge

- Status: Accepted
- Date: 2026-06-28
- Updated: 2026-07-05 (hooks are classified per-file vs. whole-workspace; whole-workspace
  hooks run isolated against a non-destructive staged snapshot — see ADR 0019)

## Context

The original `poly` Git-hooks integration shipped as a bridge: `.pre-commit-config.yaml` →
temp YAML serialization → external `prek` subprocess lookup. This approach had two
drawbacks: (1) users still maintained `.pre-commit-config.yaml` as a separate config file,
and (2) the hook runner was an external subprocess, adding latency and complicating
dependency management.

As `poly` grows into an umbrella (ADR 0011), git-hooks must be driven by `poly.toml`
(ADR 0006) and integrated as a first-class `poly hooks` subcommand. A native, in-process
hook runner is the natural fit.

## Decision

- **Implement `poly-hooks` (a new crate) as a synchronous, in-process git-hook runner.**
  It is driven by the `[hooks]` section of `poly.toml` and invoked via `poly hooks
  hook-impl` (the git-hook shim installed by `poly hooks install`).
- **Derive execution primitives, git helpers, file identification, and PTY handling from
  prek (MIT).** Keep the port minimal; refactor only to fit the sync/rayon model (we have
  no async inside the hook runner).
- **Lower `[hooks]` config into the runner's internal model:** builtin hooks (e.g. `typos`,
  `trailing-whitespace`), inline jobs (lefthook-style shell commands), and a
  `[hooks.always]` pseudo-stage for commands that run everywhere.
- **Prioritize builtins over inline commands.** Run hooks in priority groups (pre-commit →
  commit-msg → pre-push, etc.); only one hook per file match (first match wins).
- **Per-file vs. whole-workspace hooks (amendment, 2026-07-05).** Most hooks receive the
  staged file list. A hook marked `workspace = true` (and the `cargo` builtin group)
  instead analyses the whole project and runs isolated against a non-destructive staged
  snapshot rather than the live worktree; see ADR 0019 for the isolation mechanism.
- **Install git-hook shims via `poly hooks install`.** Each git stage gets a shim that
  invokes `poly hooks hook-impl` with the stage name and stdin (modified files). No
  forking of the full poly binary per stage; `hook-impl` is lightweight.
- **Retain the vendored prek copy (`crates/polyhooks`) until `poly-hooks` is complete.**
  Once the native runner ships, remove the vendor copy.

## Consequences

Positive:

- `.pre-commit-config.yaml` is eliminated for `poly` users; `[hooks]` in `poly.toml` is
  the single source of truth.
- Hook execution is in-process, synchronous, and rayon-driven; no subprocess overhead,
  consistent error handling, and tight integration with the cache (ADR 0008).
- Hooks are now part of the unified `poly` identity; the same binary handles lint,
  format, hooks, and commit-message linting.

Negative / risks:

- Porting prek's logic (especially PTY handling and complex shell integration) requires
  discipline; gaps in the port could cause hook behavior differences.
- Inline hooks (user shell scripts) are only as correct as the user; misconfigured
  inputs can cause cache-correctness issues if the hook has hidden dependencies.
- The `polyhooks` vendor copy must stay in sync until it is removed, adding temporary
  maintenance burden.

## Alternatives considered

- **Keep the prek bridge indefinitely:** rejected — it keeps `.pre-commit-config.yaml` as
  a second config file, defeating the unification goal.
- **Use a WASM sandbox for hooks:** rejected — adds complexity and a runtime; sync Rust
  is simpler and sufficiently safe for v1.
- **Async hook runner (tokio):** rejected — hooks are I/O-bound but short-lived; rayon
  offers simpler concurrency and integrates better with the per-file runner (ADR 0009).
