---
priority: medium
description: "Per-file tier vs the whole-project phase (--no-workspace), staged isolation (ADR 0019), and hierarchical poly.toml in monorepos (ADR 0018)"
---

# poly Tiers and Scope

## Per-file tier vs whole-project phase

`poly lint` runs in two phases:

- **Per-file tier** — the parallel, cached engine pass over every discovered file (native
  backends + the tree-sitter generic tier). This is all `--no-workspace` runs.
- **Whole-project phase** — invokes the same whole-workspace tools a pre-commit hook would
  (`cargo clippy` / `cargo-sort` / `cargo-machete` / `cargo-deny`, plus configured
  type checkers) on the live worktree, folding their pass/fail into the report and exit
  code. On by default; disable with `--no-workspace` or `[lint] workspace = false`. A repo
  with no `[hooks]` section runs only the per-file tier.

**Plain `poly lint` is not read-only.** poly applies no fixes without `--fix`, but the
whole-project phase *executes* those tools against the live worktree, and their own side
effects are not poly's to control (`cargo clippy` populating `target/` or refreshing
`Cargo.lock`, a `go` invocation appending to `go.work.sum`, a type checker writing its cache).
Only `--no-workspace` / `[lint] workspace = false` gives a run that cannot touch the tree.

The phase is also **not path-scoped**: `poly lint <paths>` skips it for that reason, and
`--workspace` opts back in with the tools covering the whole repository — regardless of the
named paths and of `[discovery] exclude`, which filters poly's own discovery only. A note on
stderr says which branch was taken.

Under `--fix`, the whole-project phase runs the tools in fix mode (`cargo sort` in place,
`cargo-machete --fix`, `cargo clippy --fix --allow-dirty --allow-staged`; `cargo deny` stays
check-only). Fix mode is what `--fix` adds — the phase runs either way, so `--no-workspace`,
not the absence of `--fix`, is what skips it. The git-hook / commit-gate path never requests
fix mode. `poly fmt` never runs this phase.

## Staged isolation (ADR 0019)

On the commit-gate path, poly lints the **staged** snapshot rather than the dirty worktree,
so unstaged edits neither hide nor cause failures. The staged snapshot lives in the per-user
OS cache dir, not in-repo.

## Hierarchical poly.toml in monorepos (ADR 0018)

Config resolves hierarchically: a nested `poly.toml` layers on top of ancestor configs for
files beneath it, so each package in a monorepo can tune rules while sharing a root baseline.
`poly.local.toml` remains the final local override layer, and a top-level `extends` list
(ADR 0020) shares sections from local or pinned-remote base configs.
