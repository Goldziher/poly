# Architecture Decision Records — poly

This directory is the **source of truth** for the architectural decisions behind **poly**
(polylint, polyfmt, poly-hooks, poly-commit) — a self-contained, zero-dependency Rust
binary family that replaces a repository's entire toolchain: linters, formatters,
git-hooks, and commit-message linters, all driven by one `poly.toml` config.

These ADRs are authoritative. When code, plans, or memory disagree with an accepted ADR,
the ADR wins; changing a decision means adding or superseding an ADR here, not editing code
and leaving the record stale. Each ADR uses a lightweight MADR-style format: Title, Status,
Context, Decision, Consequences (positive and negative/risks), and Alternatives considered.

## Index

| ADR | Title | Status |
|-----|-------|--------|
| [0001](0001-mission-and-scope.md) | Mission and Scope: Two Binaries Replace the Toolchain | Accepted |
| [0002](0002-pure-rust-no-subprocess.md) | Pure-Rust, No-Subprocess: Two Scoped Exceptions | Accepted |
| [0003](0003-dependency-policy.md) | Dependency Policy: Pinned Git Deps, Prebuilt Distribution | Accepted |
| [0004](0004-two-tier-coverage-architecture.md) | Two-Tier Coverage Architecture | Accepted |
| [0005](0005-backend-selections.md) | Native Backend (Tier-1) Selections | Accepted |
| [0006](0006-configuration.md) | Configuration: Canonical poly.toml, YAML Auto-Detected | Accepted |
| [0007](0007-opinionated-defaults.md) | Opinionated Defaults: Tool Defaults Plus a Thin Override Layer | Accepted |
| [0008](0008-caching.md) | Caching: Two-Tier, CACHE_FORMAT_VERSION, Hook Soundness | Accepted |
| [0009](0009-parallelism.md) | Parallelism: rayon Over Files, Saturate All Cores | Accepted |
| [0010](0010-distribution-and-naming.md) | Distribution, Naming, and Pre-Commit Integration | Accepted |
| [0011](0011-poly-umbrella.md) | The poly Umbrella: One Binary Family, One Config | Accepted |
| [0012](0012-native-hook-runner.md) | Native Hook Runner Replaces the prek Bridge | Accepted |
| [0013](0013-catalog-tool-config.md) | Catalog-Driven Tool Config: Retire .pre-commit-config.yaml | Accepted, pending impl |
| [0014](0014-toolchain-interop.md) | Toolchain Interop: Capability-Probed First-Party CLIs | Accepted |

## Conventions

- Files are named `NNNN-kebab-title.md`, numbered from `0001`.
- Status is one of: Proposed, Accepted, Superseded (by NNNN), Deprecated.
- New decisions get the next number; revisions to an accepted decision are made by a new
  ADR that supersedes the old one (mark the old one `Superseded by NNNN`).
- **Amendments to accepted decisions** (bug fixes, clarifications, or evolving context
  that doesn't change the core decision) are noted with an "Updated: YYYY-MM-DD"
  timestamp and a brief note after the Date field, without incrementing the ADR number.
