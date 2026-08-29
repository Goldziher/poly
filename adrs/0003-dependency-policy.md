# 0003 — Dependency Policy: Wrap First, Vendor Only When Forced

- Status: Accepted
- Date: 2026-06-26
- Updated: 2026-06-28 (pivot to pinned git deps, no crates.io publish, new vendoring
  exceptions)
- Updated: 2026-08-29 (ruff is no longer a git pin — see "Amendment — 2026-08-29" below)

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
GitHub release (see ADR 0010), plus installers (`curl | sh` and PowerShell), a Homebrew
formula, and the `Goldziher/poly` GitHub Action. We do **not** publish crates to crates.io;
pinned git deps are only possible because we distribute binaries.

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

## Amendment — 2026-08-29 (ruff migrated to crates.io)

Point 2 of the Decision names **ruff** as an example of a tool that ships its useful logic only in
a monorepo. That is no longer true, and the strict ordering did its job: the moment a published
crate became available, the git pin lost its justification.

Astral began publishing the ruff workspace to crates.io on **2026-06-23** and has published every
weekly release since — eleven releases, zero yanks — as an automated step of the release pipeline
(`scripts/publish-crates.py` enumerates workspace members via `cargo metadata` and skips only
`publish = false` packages; none of the crates poly uses carries that key). poly now depends on
the registry:

```toml
ruff_linter = "=0.16.5"
ruff_db = "=0.0.11"          # and ruff_formatter, ruff_python_ast,
                             # ruff_python_formatter, ruff_text_size
```

Three consequences worth recording:

- **The `=` is deliberate, not cargo pedantry.** These are private-by-intent internals that ruff
  does not treat as a public API and does not semver. A caret range on `0.16.x` would let
  `cargo update` walk into unannounced breakage between releases — exactly the failure the git
  `rev` prevented. An exact pin keeps that property.
- **`Engine::version()` had to change with the dependency.** `RuffEngine::version()` embedded the
  git rev (`git-ruff:700421c+…`) and now embeds `ruff-0.16.5+…`. Had it not changed, the
  content-hash cache (ADR 0008) would have served results computed by the old rev under the new
  dependency — a stale-cache correctness bug that no test run would reveal.
- **`deny.toml`'s `allow-git` shrinks with each retired pin.** `https://github.com/astral-sh/ruff`
  was removed. Leaving it would be a permission granted to nothing — the same dead-configuration
  class ADR 0016's amendment describes.

Three `[workspace.dependencies]` entries (`ruff_diagnostics`, `ruff_python_parser`,
`ruff_source_file`) were referenced by no member manifest and no source file, and were deleted.
`cargo machete` does not catch unused *workspace* dependency keys, which is why they survived.

Migrating cost 42 upstream commits — the pin sat that far past the `0.16.5` tag. The diff across
the nine crates is 21 files of rule bodies, snapshots and two `generate.py`, touching **no API
poly calls**; the next weekly release restores them.

### The other three pins stay, and are not the same case

- **oxc** — four of the seven crates poly uses (`oxc_formatter`, `oxc_formatter_core`,
  `oxc_formatter_json`, `oxc_linter`) carry `publish = false` and are permanently unpublishable.
  A *partial* migration is a hard compile error, not merely poor practice: `oxc_formatter::format`
  takes an `oxc_allocator::Allocator` and an `oxc_span::SourceType`, and cargo treats a registry
  crate and a git crate as distinct instances — the registry `SourceType` would not typecheck
  against the git `oxc_formatter`'s own. That is the same-`rev` rule in point 2, and it binds the
  whole monorepo together. (The `oxc_formatter` name on crates.io is an abandoned 2023 crate, all
  eight versions yanked, unrelated to today's oxfmt.)
- **biome** — half the crates poly uses are unpublished, and the published half is a version-skew
  trap: `biome_analyze` declares `0.5.7` both on crates.io and at our pinned rev, but the
  published tarball's `.cargo_vcs_info.json` names a commit from **2024-03-12** against a pin from
  2026-08-29. Cargo would accept `"0.5.7"` without complaint and hand us two-and-a-half-year-old
  code under an identical version number. Biome's publish workflow is `workflow_dispatch`-only and
  was last run in Dec 2024.
- **rubyfmt** — the crates.io `rubyfmt` is a name reservation: one version `0.0.0-wip`, 1,549
  bytes, whose entire `src/main.rs` prints `Hello, world!`. The real library is `librubyfmt/`
  (crate name `rubyfmt`, version 0.14.1) and has never been published.

**The standing rule this adds:** a git pin records upstream's *publishing posture at a moment in
time*, not a permanent property. Re-check the pins when touching dependencies. The check is cheap
— query `https://crates.io/api/v1/crates/<name>` and compare the published version against the
version the crate declares at our pinned rev — and a name existing on crates.io is not enough:
verify it is not a placeholder, a yanked squat, or the same version number over far older code.
