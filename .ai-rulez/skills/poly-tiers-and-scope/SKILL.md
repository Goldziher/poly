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

Under `--fix`, the whole-project phase runs the tools in fix mode (`cargo sort` in place,
`cargo-machete --fix`, `cargo clippy --fix --allow-dirty --allow-staged`; `cargo deny` stays
check-only). Only `poly lint --fix` runs it; the git-hook / commit-gate path is always
check-only. `poly fmt` never runs this phase.

## Staged isolation (ADR 0019)

On the commit-gate path, poly lints the **staged** snapshot rather than the dirty worktree,
so unstaged edits neither hide nor cause failures. The staged snapshot lives in the per-user
OS cache dir, not in-repo.

## Hierarchical poly.toml in monorepos (ADR 0018)

Config resolves hierarchically: a nested `poly.toml` layers on top of ancestor configs for
files beneath it, so each package in a monorepo can tune rules while sharing a root baseline.
`poly.local.toml` remains the final local override layer, and a top-level `extends` list
(ADR 0020) shares sections from local or pinned-remote base configs.
