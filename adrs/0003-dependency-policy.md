# 0003 — Dependency Policy: Wrap First, Vendor Only When Forced

- Status: Accepted
- Date: 2026-06-26
- Updated: 2026-06-28 (pivot to pinned git deps, no crates.io publish, new vendoring
  exceptions)

## Context

Given the no-subprocess constraint (ADR 0002), every backend tool must be reached through
Rust crates. Upstream crates vary: some expose clean library APIs, some bury the useful
logic behind a binary `main` and don't externalize a usable entry point, and some are
fast-moving. We need one consistent rule for how we depend on them.

## Decision

A strict ordering:

1. **Prefer published crates on crates.io.** If an upstream crate (e.g. `taplo`,
   `sqruff_lib`, `rumdl_lib`) externalizes the API we need, depend on it directly and
   write a thin `Engine` adapter (ADR 0004).
2. **Use pinned git `rev` for monorepo internals.** When a tool (ruff, oxc) ships useful
   logic only in its monorepo and not as a published crate, depend on the GitHub repo
   pinned to a specific `rev` (commit) for reproducibility. When multiple crates come
   from one monorepo (e.g. all of oxc), pin them to the **same `rev`** to keep internal
   versions consistent.
3. **No version vendoring in a `vendor/` directory.** Do not maintain a forked copy of
   upstream source. Pinned git deps track upstream without duplication and are fine since
   we do not publish our own crates to crates.io (see below).
4. **Two documented vendoring exceptions (see `ATTRIBUTIONS.md`):**
   - **prek (derived & vendored into `crates/poly-hooks`):** git-hook execution primitives,
     ported to sync/rayon form. Vendored copy `crates/polyhooks` retained during
     migration, removed once inlined.
   - **mdsf tool-catalog data:** tool-definition JSON (tool → binary → argv → stdin →
     languages) and fixtures, vendored to populate the built-in tool registry without a
     crate dependency.

**Distribution note:** poly ships **prebuilt, platform-specific binaries** attached to a
GitHub release (see ADR 0010), plus an installer (`curl | sh` / npm / pip / cargo-binstall).
We do **not** publish crates to crates.io; pinned git deps are only possible because we
distribute binaries.

Each backend begins with an **empirical API check**: clone the crate to `/tmp` at the
exact `rev`, confirm the library API, then wrap or decide on a git dep.

A `cargo deny` license gate guards the full dependency tree (crates.io + git deps and
their transitive deps): no GPL/AGPL. All vendored sources are recorded in `ATTRIBUTIONS.md`
with their license and copyright.

## Consequences

Positive:

- Published crates get upstream bug fixes automatically.
- Pinned git `rev` provides reproducible, auditable dependencies without maintaining a
  fork. Commits pinned in `Cargo.toml` are easy to review and track.
- Binary distribution means git-rev dependencies are fine — they're unavailable in
  crates.io only because crates.io forbids publishing with them.
- `Cargo.lock` commits make builds reproducible; `cargo deny` gates licenses.
- Bounded vendoring (two exceptions, both documented in `ATTRIBUTIONS.md`) keeps the
  maintenance surface minimal.

Negative / risks:

- Pinned git `rev`s mean we're not automatically advanced by `cargo update`; upstream
  changes must be reviewed and explicitly pulled (a feature, not a bug, for this use case).
- The prek port into `poly-hooks` requires ongoing maintenance as prek evolves.
- The mdsf catalog data must be kept in sync if we want new tools; updates are manual.

## Alternatives considered

- **Vendor everything for stability:** rejected — turns the project into a permanent fork
  of a dozen tools; unmaintainable.
- **Unpin all git deps / use semver ranges:** rejected — the resulting churn and
  non-determinism is unacceptable for a dev tool that must be reproducible across a
  team's development and CI environments. Pinning is a strength for tools, not a
  weakness.
- **Publish crates to crates.io:** rejected — it forces unpinning, which then forces
  vendoring; binary distribution is the right boundary.
