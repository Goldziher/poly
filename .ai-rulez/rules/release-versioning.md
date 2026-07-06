---
priority: high
---

# Release Versioning & Distribution

poly ships **a single binary — `poly`** (driven by one config), distributed the way ruff /
oxlint / biome are: **prebuilt, platform-specific binaries attached to a GitHub release, plus an
installer**. The channels are the `curl | sh` / PowerShell installer, the GitHub Action
(`Goldziher/poly@v0`), and Homebrew (`brew install Goldziher/tap/poly`). The release artifacts
are named `poly-<version>-<triple>`. We do **NOT publish to crates.io** — our crates depend on
pinned git dependencies (oxc, ruff internals), which crates.io forbids in published crates, and
we have no need for a source-distribution channel. There are **no npm or PyPI wrapper packages**
(they were removed).

Versioning is **lock-step**: bump all surfaces to the same `X.Y.Z` in one change. The single
source of truth is the workspace `[workspace.package] version` in the root `Cargo.toml`; member
crates inherit it via `version.workspace = true`.

## Rules

- Tags MUST be `v<version>` (e.g. `v0.1.0`); the release build runs from the tagged commit.
- The release workflow cross-compiles binaries (Linux / macOS / Windows × arch) and uploads
  them to the GitHub release; the installer, GitHub Action, and Homebrew formula fetch the
  artifact matching the host.
- We do not publish to crates.io; the binary is the only distribution surface.
- Commit `Cargo.lock` and pin git-dependency `rev`s so a tagged build is reproducible.
</content>
