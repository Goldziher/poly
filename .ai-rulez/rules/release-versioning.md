---
priority: high
---

# Release Versioning

polylint publishes **two binary crates — `polylint` and `polyfmt`** (plus `polylint-core` if
it is published rather than kept path-only). They are versioned in **lock-step**: bump all
published surfaces to the same `X.Y.Z` in the same change. The single source of truth is the
workspace `[workspace.package] version` in the root `Cargo.toml`; the member crates inherit it
via `version.workspace = true`.

## Surfaces

| Surface | Format | Notes |
|---|---|---|
| `Cargo.toml` `[workspace.package] version` | `X.Y.Z` | Source of truth; inherited by all members. |
| `polylint` (crates.io) | `X.Y.Z` | Published lint binary. |
| `polyfmt` (crates.io) | `X.Y.Z` | Published format binary. |
| `polylint-core` (crates.io) | `X.Y.Z` | Only if published; otherwise path-only. |

## Rules

- crates.io enforces version uniqueness, so never re-publish an already-published `X.Y.Z`;
  bump first.
- Names were reserved at `v0.0.1`; subsequent releases follow standard semver.
- Tags MUST be `v<version>` (e.g. `v0.1.0`). Publish from the tagged commit.
- Keep the two binaries on the same version even when only one changed — a single config drives
  both, and users install them as a pair.
</content>
