---
priority: high
---

# Release Versioning & Distribution

polylint ships **two binaries — `polylint` and `polyfmt`** (driven by one config), distributed
the way ruff / oxlint / biome are: **prebuilt, platform-specific binaries attached to a GitHub
release, plus an installer** (a `curl | sh` script and/or npm / pip / `cargo-binstall`
wrappers). We do **NOT publish to crates.io** — our crates depend on pinned git dependencies
(oxc, ruff internals), which crates.io forbids in published crates, and we have no need for a
source-distribution channel.

Versioning is **lock-step**: bump all surfaces to the same `X.Y.Z` in one change. The single
source of truth is the workspace `[workspace.package] version` in the root `Cargo.toml`; member
crates inherit it via `version.workspace = true`.

## Rules

- Tags MUST be `v<version>` (e.g. `v0.1.0`); the release build runs from the tagged commit.
- Keep `polylint` and `polyfmt` on the same version even when only one changed — one config
  drives both and users install them as a pair.
- The release workflow cross-compiles binaries (Linux / macOS / Windows × arch) and uploads
  them to the GitHub release; the installer fetches the artifact matching the host.
- The `polylint` / `polyfmt` names reserved on crates.io at `v0.0.1` are **not** re-published;
  the reservation only holds the names.
- Commit `Cargo.lock` and pin git-dependency `rev`s so a tagged build is reproducible.
</content>
