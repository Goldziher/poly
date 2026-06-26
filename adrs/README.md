# Architecture Decision Records — polylint / polyfmt

This directory is the **source of truth** for the architectural decisions behind
**polylint** (lint) and **polyfmt** (format) — two self-contained, zero-dependency Rust
binaries that replace a repository's entire per-language linter/formatter toolchain.

These ADRs are authoritative. When code, plans, or memory disagree with an accepted ADR,
the ADR wins; changing a decision means adding or superseding an ADR here, not editing code
and leaving the record stale. Each ADR uses a lightweight MADR-style format: Title, Status,
Context, Decision, Consequences (positive and negative/risks), and Alternatives considered.

## Index

| ADR | Title | Status |
|-----|-------|--------|
| [0001](0001-mission-and-scope.md) | Mission and Scope: Two Binaries Replace the Toolchain | Accepted |
| [0002](0002-pure-rust-no-subprocess.md) | Pure-Rust, No-Subprocess, No-System-Dependency Constraint | Accepted |
| [0003](0003-dependency-policy.md) | Dependency Policy: Wrap First, Vendor Only When Forced | Accepted |
| [0004](0004-two-tier-coverage-architecture.md) | Two-Tier Coverage Architecture | Accepted |
| [0005](0005-backend-selections.md) | Native Backend (Tier-1) Selections | Accepted |
| [0006](0006-configuration.md) | Configuration: Canonical polylint.toml, YAML Auto-Detected | Accepted |
| [0007](0007-opinionated-defaults.md) | Opinionated Defaults: Tool Defaults Plus a Thin Override Layer | Accepted |
| [0008](0008-caching.md) | Caching: blake3 Content-Hash, Atomic Writes, Supersedes Tool Caches | Accepted |
| [0009](0009-parallelism.md) | Parallelism: rayon Over Files, Saturate All Cores | Accepted |
| [0010](0010-distribution-and-naming.md) | Distribution, Naming, and Pre-Commit Integration | Accepted |

## Conventions

- Files are named `NNNN-kebab-title.md`, numbered from `0001`.
- Status is one of: Proposed, Accepted, Superseded (by NNNN), Deprecated.
- New decisions get the next number; revisions to an accepted decision are made by a new
  ADR that supersedes the old one (mark the old one `Superseded by NNNN`).
