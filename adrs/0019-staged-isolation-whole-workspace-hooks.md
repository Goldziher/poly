# 0019 — Staged Isolation for Whole-Workspace Hooks

- Status: Accepted
- Date: 2026-07-05

## Context

The native hook runner (ADR 0012) runs each hook over the **staged file list**. That is
correct for per-file tools (`polylint`, `polyfmt`, catalog formatters), but a class of tools
analyses the *whole project* at once and cannot be scoped to a file list: `cargo clippy`,
type checkers like `pyrefly` / `mypy` / `tsc`, `golangci-lint`. ADR 0014 explicitly ruled
these "project-wide" tools out of the per-file native-toolchain model and deferred them —
leaving no home for them.

Run as pre-commit hooks, these tools have a second problem: they compile/analyse the **live
worktree**, so a commit is gated against unstaged edits and untracked files rather than
against what is actually being committed. Partially-staged files leak their dirty content
into the check. The pre-commit framework and its fork `prek` solve this by stashing the
worktree (`git stash` / `git checkout -- .`), which is **destructive** — a crashed run or an
autofix/stash conflict can lose uncommitted work. That failure mode is unacceptable.

## Decision

- **Per-file vs. whole-workspace hook classification.** A hook carries `workspace: bool`
  (`[hooks.<stage>.commands.<job>] workspace = true`; the `cargo` builtin group sets it
  intrinsically). Per-file hooks are unchanged. A whole-workspace hook takes **no appended
  filenames** (`workspace = true` ⇒ `pass_filenames = false`; a `{staged_files}` template
  opts back in), because it operates on the whole project — e.g. `pyrefly check
  packages/python`.
- **Non-destructive staged snapshot.** Whole-workspace hooks run against a copy of the git
  **index** materialized with `git checkout-index`, not the live worktree. The worktree is
  never mutated — no stash, no `checkout -- .`. Untracked files and unstaged edits are absent,
  so the hook sees exactly what the commit would capture.
- **Persistent, mtime-faithful cache, not an ephemeral dir.** The snapshot is a managed,
  git-ignored cache at `<repo>/.polylint/staged`, refreshed in place each run: tracked files
  whose worktree equals the index are copied **preserving their worktree mtime** (skipped
  when already up to date), so each tool's native incremental cache — cargo's `target/`,
  `.mypy_cache`, tsc build-info — stays warm; only files whose worktree differs from the index
  (plus symlinks) are rewritten from the staged blob; a manifest-based prune removes files
  that left the tree while preserving tool caches inside the snapshot. Cargo is pointed at the
  real repo `target/` (`CARGO_TARGET_DIR`): cargo namespaces artifacts by a metadata hash that
  includes the crate source path, so snapshot-root and dev-root builds **coexist** without
  overwriting, and registry-dependency artifacts (path-independent) are shared.
- **Default-on for commit-gating stages; off for whole-tree runs.** Isolation is active for
  the index stages (`pre-commit`, `pre-merge-commit`) and skipped for `--all-files` (which
  deliberately checks the whole tree) and non-index stages. `[hooks] isolate = false` forces
  it off; a snapshot is only built when the stage actually contains a whole-workspace hook.
- **Cache correctness under isolation (ADR 0008).** A whole-workspace hook's result-cache key
  digests **staged** bytes (read from the snapshot), while the input *file set* is resolved
  from the real repository. Keying on the worktree instead would allow a false hit — reverting
  an unstaged edit could replay a pass computed against different staged content.

## Consequences

Positive:

- Project-wide tools finally have a first-class home (closing the ADR 0014 deferral): they run
  as whole-workspace hooks, gated on staged content, with no per-file contortions.
- Non-destructive by construction — the worktree is never touched, eliminating the entire
  class of stash/restore data-loss failures that make `pre-commit`/`prek` risky.
- The persistent mtime-faithful snapshot keeps every tool's incremental cache warm, so
  isolation does not force cold rebuilds on each commit.
- Combined with result caching, a commit touching no Rust skips the whole `cargo` group, and a
  Python-only commit skips it too — polyglot repos pay only for what changed.

Negative / risks:

- A second on-disk copy of the tracked tree (`.polylint/staged`) plus, for cargo, coexisting
  workspace-crate artifacts in `target/` — a disk cost proportional to repo size. It is
  git-ignored, pruned, and purgeable (`rm -rf .polylint/staged`).
- The cleanup model shifts from "deleted every run" to a **managed cache**: bounded and
  self-healing (a crash mid-refresh is corrected next run), but persistent by design, which
  users must understand.
- Single-writer is assumed; concurrent `poly hooks` runs on one repo are not yet locked
  (matching the result cache's current posture, ADR 0008).
- A whole-workspace *formatter* run under isolation writes fixes into the snapshot, not the
  worktree; the autofix write-back path is out of scope for this ADR (the tools in scope —
  clippy, type checkers — are check-only).

## Alternatives considered

- **prek/pre-commit stash-the-worktree (`git stash` / `git checkout -- .`):** rejected —
  destructive; a crash or stash/autofix conflict can lose uncommitted work. Non-destructive
  isolation was a hard requirement.
- **`gix` (gitoxide) for the checkout:** rejected — the snapshot is a once-per-run, disk-I/O-
  bound operation, not a hot loop, so `gix`'s in-process advantage is marginal here, while it
  would add ~100–200 transitive crates (against the lean-binary goal, ADR 0001/0003), a
  plumbing-level API, and filter/attribute handling that is "base implementations" rather than
  the reference `git`. Subprocess `git checkout-index` is the reference implementation for
  exec-bits/symlinks/CRLF/`.gitattributes` and is consistent with the git subprocess the hook
  runner already uses (ADR 0002's scoped exception). Reconsider only if the checkout ever
  becomes per-file hot-path or must work with no `git` binary present.
- **Ephemeral per-run snapshot (delete after each run):** rejected — a fresh checkout resets
  every file's mtime, forcing cargo/type-checkers to rebuild the whole workspace each commit.
  The persistent mtime-faithful cache is what makes isolation cheap.
- **Dedicated `CARGO_TARGET_DIR` per snapshot instead of the real `target/`:** rejected —
  cargo already namespaces artifacts by source-path hash, so sharing the real `target/` reuses
  all dependency compilation and coexists with dev builds without thrash; a dedicated target
  would recompile every dependency on the first isolated run.
- **Isolate every hook (including per-file formatters):** rejected for now — per-file hooks
  already receive the staged file list; extending the snapshot to them buys partial-staging
  correctness but drags in non-destructive autofix write-back into the index, a larger,
  trickier change deferred until measured need.
