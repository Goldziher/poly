# 0019 — Staged Isolation for the Commit Gate

- Status: Accepted
- Date: 2026-07-05
- Updated: 2026-07 (v0.9.0): the staged snapshot moved out of the in-repo `.polylint/staged`
  into the per-user OS cache dir (`~/.cache/poly/<repo-key>/staged`), alongside the result
  cache (ADR 0008).
- Updated: 2026-07-07: the same whole-workspace tool set now also runs as a phase of
  `poly lint` (on by default), against the **live worktree** — not the staged snapshot, since
  `poly lint` checks the working tree rather than gating a commit. See the "poly lint
  whole-project phase" note below.
- Updated: 2026-08-12: isolation was extended from whole-workspace hooks to **every** hook in a
  commit-gating run, closing a false-pass. See "Extension: every hook, not just the
  whole-workspace ones" below.

## Context

The native hook runner (ADR 0012) runs each hook over the **staged file list**. That is
correct for per-file tools (`lint`, `fmt`, catalog formatters), but a class of tools
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
- **Persistent, index-sourced cache, not an ephemeral dir.** The snapshot is a managed
  cache in the per-user OS cache dir (`~/.cache/poly/<repo-key>/staged`), refreshed in place
  each run. Content is
  sourced **only from the index blob** (`git checkout-index`), never copied from the worktree,
  so an unstaged edit can never leak in — correctness does not depend on git's stat cache (see
  the rejected alternative below). A path is re-materialized only when its **index OID changed**
  since the last snapshot (or its snapshot copy is missing), tracked by a `path → OID` manifest;
  unchanged paths are left untouched, so their mtime is stable across runs and each tool's native
  incremental cache — cargo's `target/`, `.mypy_cache`, tsc build-info — stays warm. The same
  manifest drives a prune of files that left the tree while preserving tool caches inside the
  snapshot. Cargo is pointed at the real repo `target/` (`CARGO_TARGET_DIR`): cargo namespaces
  artifacts by a metadata hash that includes the crate source path, so snapshot-root and dev-root
  builds **coexist** without overwriting, and registry-dependency artifacts (path-independent)
  are shared.
- **Default-on for commit-gating stages; off for whole-tree runs.** Isolation is active for
  the index stages (`pre-commit`, `pre-merge-commit`) and skipped for `--all-files` (which
  deliberately checks the whole tree) and non-index stages. `[hooks] isolate = false` forces
  it off; a snapshot is only built when the stage actually contains a whole-workspace hook.
- **Cache correctness under isolation (ADR 0008).** A whole-workspace hook's result-cache key
  digests **staged** bytes (read from the snapshot), while the input *file set* is resolved
  from the real repository. Keying on the worktree instead would allow a false hit — reverting
  an unstaged edit could replay a pass computed against different staged content.
- **`poly lint` whole-project phase (2026-07-07).** `poly lint` runs a whole-project phase
  after its per-file tier: it lowers the `pre-commit` stage, keeps only the `workspace = true`
  hooks (the `cargo` builtin group + inline whole-project jobs — one source of truth with the
  hooks config), and runs them via the same `poly_hooks::run` engine, folding their pass/fail
  into the lint report and exit code. It runs against the **live worktree** (`work_root = None`)
  because `poly lint` inspects the working tree, not the index, so no staged snapshot is built.
  On by default; `--no-workspace` (or `[lint] workspace = false`) opts out. The `lint` hook
  builtin invokes `poly lint --no-workspace`, so a `poly hooks` run never double-runs these
  tools (the `cargo` group already covers them).

## Extension: every hook, not just the whole-workspace ones (2026-08-12)

The original decision isolated only `workspace = true` hooks and explicitly deferred the rest
("Isolate every hook (including per-file formatters)" under *Alternatives considered*). That left
the two tiers **disagreeing inside a single `poly hooks run pre-commit`**: a whole-workspace hook
read the index while a per-file hook read the worktree, and nothing in the report said so. The
consequence was a false pass — the most severe class of gate defect. Reproduced end to end: stage
a file containing a violation, fix only the worktree copy, `git commit`; the per-file hook
validated the clean worktree bytes, reported ✓, and the violating **staged** bytes landed in
`HEAD`. The mirror case was just as wrong: an unstaged edit blocked a commit whose staged content
was fine.

- **One run, one tree.** When a run carries a staged snapshot, *every* hook executes from it —
  per-file and whole-workspace alike. Whether a run is staged-scoped is unchanged and remains a
  property of the run, not of the hook: the index stages (`pre-commit`, `pre-merge-commit`), not
  `--all-files`, not non-index stages, `[hooks] isolate` overriding. `poly hooks run pre-commit
  --all-files` is a question about the working tree and still answers it.
- **The snapshot is built whenever the stage has hooks**, not only when one is `workspace = true`.
  It is still built once per run and refreshed incrementally from index OIDs — never per file or
  per hook.
- **The report names the tree.** Every hook outcome records which tree produced its verdict, and
  the stage banner renders it (`[stage] pre-commit — validated staged content`). Silence about
  which bytes were checked was the underlying defect; a mixed run would render as such rather
  than pick one.
- **Partially-staged files** (`git add -p`) are judged by their staged version alone. An unstaged
  hunk can neither pass nor fail the gate.
- **Autofix write-back** (the case the original ADR left out of scope). A hook's rewrite lands in
  the snapshot, so it must be carried back or it is lost. It is carried back **only where the
  worktree copy is byte-identical to the index** — there, the worktree holds nothing the fix has
  not already seen, so writing it destroys nothing and reproduces the pre-isolation result. Where
  the worktree copy differs, the author holds unstaged work: overwriting it would destroy that
  work and `git add`ing the file would stage hunks they deliberately left out. So the fix is
  **withheld**, and for a `stage_fixed` hook that fails the run and blocks the commit — the only
  outcome that loses nothing. Reconciliation happens once per priority group, because the hooks
  in a group run concurrently and a per-hook pass would let one hook's write make another's
  write-back look unsafe.
- **Detection is content-based** (blake3 over each matched file before and after), not stat-based:
  a formatter rewriting a file to the same length inside one mtime tick is ordinary, and a missed
  rewrite means either a lost fix or a cached "passed" for content that never passed on its own.

Accepted consequences of the wider scope:

- A hook's **untracked side effects** (logs, generated files not in its matched set) are written
  into the snapshot and discarded on the next refresh, where previously they appeared in the
  worktree. Anything a hook is expected to *deliver* must be one of its matched files, which is
  also what `stage_fixed` already required to be effective.
- **Untracked or unstaged config is not visible to hooks** under the gate — notably a gitignored
  `poly.local.toml`. For a commit gate this is arguably correct (local overrides should not weaken
  a shared gate), but it is a behaviour change for anyone relying on them there. `[hooks] isolate
  = false` restores worktree scoping.

## Consequences

Positive:

- Project-wide tools finally have a first-class home (closing the ADR 0014 deferral): they run
  as whole-workspace hooks, gated on staged content, with no per-file contortions.
- Non-destructive by construction — the worktree is never touched, eliminating the entire
  class of stash/restore data-loss failures that make `pre-commit`/`prek` risky.
- The persistent, OID-incremental snapshot keeps every tool's incremental cache warm, so
  isolation does not force cold rebuilds on each commit.
- Combined with result caching, a commit touching no Rust skips the whole `cargo` group, and a
  Python-only commit skips it too — polyglot repos pay only for what changed.

Negative / risks:

- A second on-disk copy of the tracked tree (`~/.cache/poly/<repo-key>/staged`) plus, for
  cargo, coexisting workspace-crate artifacts in `target/` — a disk cost proportional to repo
  size. It lives outside the repo, is pruned, and is purgeable (`poly cache clean`).
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
- **Copy clean files from the worktree, checkout only `git diff-files`-modified files:** rejected
  after a correctness bug surfaced in sibling-repo testing. `git diff-files` is **stat-based** and
  can **under-report** a genuinely-modified file as clean when the index stat cache is stale or
  inconsistent (observed once on a real repo: an unstaged-only, size-differing file was copied
  from the worktree, leaking the unstaged edit into the snapshot; a later `git status` that
  repaired the stat cache made the next run correct). `git update-index --refresh` does not
  reliably repair every such state. Sourcing content from the **index OID** (`git ls-files -s` +
  `git checkout-index`) is stat-independent and correct by construction; the OID manifest still
  gives incremental, warm refreshes without the worktree-copy fast path.
- **Ephemeral per-run snapshot (delete after each run):** rejected — a fresh checkout of the whole
  tree each commit forces cargo/type-checkers to rebuild everything. The persistent cache
  re-materializes only OID-changed files, leaving unchanged files (and their mtimes) in place, so
  incremental caches stay warm.
- **Dedicated `CARGO_TARGET_DIR` per snapshot instead of the real `target/`:** rejected —
  cargo already namespaces artifacts by source-path hash, so sharing the real `target/` reuses
  all dependency compilation and coexists with dev builds without thrash; a dedicated target
  would recompile every dependency on the first isolated run.
- **Isolate every hook (including per-file formatters):** originally rejected as "partial-staging
  correctness at the cost of a trickier autofix write-back, deferred until measured need".
  **Reversed on 2026-08-12** — the need was not marginal: receiving the staged *file list* while
  reading *worktree bytes* is a false pass, not a partial-staging nicety. See the extension
  section above for the write-back semantics that made it tractable.
