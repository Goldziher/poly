# 0010 — Distribution, Naming, and Pre-Commit Integration

- Status: Accepted
- Date: 2026-06-26

## Context

The mission (ADR 0001) only lands if adoption is frictionless: discoverable crate names, a
clean separation between the reusable engine and the user-facing binaries, and a
pre-commit integration that actually deletes a repo's existing hook sprawl rather than
adding to it.

## Decision

- **Two binary crates: `polylint` (lint) and `polyfmt` (format)**, each a thin CLI over a
  shared **`polylint-core`** engine library (the `Engine` trait, registry, config,
  defaults, cache, runner, reporter). Core is a path dependency; publishing it separately
  is an open follow-up.
- **Names reserved on crates.io at v0.0.1** (done 2026-06-26) so the identity is locked
  before real releases.
- A Cargo **workspace** with `crates/polylint-core`, `crates/polylint`, `crates/polyfmt`,
  and an optional `vendor/` (ADR 0003).
- **Ship a `.pre-commit-hooks.yaml`** defining exactly two hooks (`polylint` and `polyfmt`)
  so a consuming repo can **replace its entire hook list with two entries** pointed at this
  repo.

## Consequences

Positive:

- Clear separation of concerns: all logic lives in `polylint-core`; the binaries stay
  trivial and the engine is reusable/embeddable.
- Reserved names prevent squatting and give a stable install story (`cargo install
  polylint polyfmt`).
- The shipped hooks file makes the headline pitch real: a repo's `.pre-commit-config.yaml`
  collapses to two hooks, deleting every per-tool hook and its system dependency.

Negative / risks:

- Two binaries share heavy dependencies (oxc, ruff, …); without care, distribution size
  and build time roughly double unless core is structured so each binary pulls only what it
  needs.
- Keeping two CLIs' UX consistent (flags, exit codes, output formats) is ongoing work.
- Whether `polylint-core` is ever published (vs. path-only) remains undecided; consumers
  wanting to embed the engine are blocked until then.

## Alternatives considered

- **Single binary with `lint`/`format` subcommands:** rejected — two binaries map directly
  onto two pre-commit hooks and keep each tool's surface focused (see ADR 0001).
- **No shared core (duplicate logic per binary):** rejected — guarantees drift and double
  maintenance.
- **Distribute only via pre-commit, no crates.io binaries:** rejected — `cargo install`
  and reserved names matter for direct/editor/CI use beyond pre-commit.
