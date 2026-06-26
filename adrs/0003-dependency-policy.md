# 0003 — Dependency Policy: Wrap First, Vendor Only When Forced

- Status: Accepted
- Date: 2026-06-26

## Context

Given the no-subprocess constraint (ADR 0002), every backend tool must be reached through
Rust crates. Upstream crates vary: some expose clean library APIs, some bury the useful
logic behind a binary `main` and don't externalize a usable entry point, and some are
fast-moving. We need one consistent rule for how we depend on them.

## Decision

A strict ordering:

1. **Wrap the published crate first.** If an upstream crate (e.g. `taplo`, `sqruff_lib`,
   `rumdl_lib`, `oxc_linter`) externalizes the API we need, depend on it directly and write
   a thin `Engine` adapter (ADR 0004).
2. **Vendor the source only if it doesn't externalize what's needed.** When the logic is
   not reachable as a library (the likely case for ruff and possibly oxfmt), copy the
   relevant source into `vendor/`, record every vendored source — upstream repo, license,
   and the exact commit — in `ATTRIBUTIONS.md`, and adapt minimally.
3. **No version pinning. No git-rev dependencies.** Depend on published crates by
   permissive semver ranges (mirroring the alef `Cargo.toml` conventions); never pin an
   exact `=x.y.z` and never point a dependency at a git revision.

Each backend begins with an **empirical API check**: clone the crate to `/tmp`, confirm
whether it externalizes what we need, then wrap or vendor accordingly.

A `cargo deny` license gate guards the tree: no GPL/AGPL pulled in, and `ATTRIBUTIONS.md`
must stay complete.

## Consequences

Positive:

- We get upstream bug fixes and improvements automatically for wrapped crates.
- Vendoring is bounded and auditable: it only happens when forced, and is always
  attributed and license-checked.
- No git-rev/pinning means a clean, reproducible-by-semver dependency graph that
  `cargo update` can advance.

Negative / risks:

- Unpinned ranges mean an upstream release can change behavior or break our build between
  CI runs; we accept this in exchange for staying current (the dry-run corpus catches
  regressions).
- Vendored code is a maintenance fork: we must periodically re-sync against upstream and
  re-record commits, or drift silently.
- **ruff has no stable public library API**, so vendoring `ruff_linter` /
  `ruff_python_formatter` / `ruff_python_parser` is the likely outcome — the largest
  vendoring commitment we expect to carry.

## Alternatives considered

- **Vendor everything for stability:** rejected — turns the project into a permanent fork
  of a dozen tools; unmaintainable.
- **Pin exact versions / use git revs:** rejected — pinning freezes us out of fixes and
  bloats the lockfile churn story; git-rev deps can't be published to crates.io anyway.
- **Fork-and-PR upstreams to export APIs:** preferred long-term where feasible, but we
  cannot block on upstream review; vendoring is the pragmatic bridge.
