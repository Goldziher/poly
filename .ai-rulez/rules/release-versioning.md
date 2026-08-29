---
priority: high
---

# Release Versioning & Distribution

poly ships **a single binary — `poly`** (driven by one config), distributed the way ruff /
oxlint / biome are: **prebuilt, platform-specific binaries attached to a GitHub release, plus an
installer**. The channels are the `curl | sh` / PowerShell installer (`install.sh` /
`install.ps1`), the GitHub Action (`Goldziher/poly@v0`, `action.yml` — which shells out to the
same `install.sh`), and Homebrew (`brew install Goldziher/tap/poly`). The release artifacts are
named `poly-<version>-<triple>.tar.gz` (`.zip` on Windows), six targets, plus a
`sha256sums.txt`. We do **NOT publish to crates.io** — our crates depend on pinned git
dependencies (**oxc, biome, rubyfmt** — see `deny.toml`'s `allow-git`), which crates.io forbids
in published crates, and we have no need for a source-distribution channel. Ruff is **not** a git
pin: since 2026-08-29 the ruff crates come from crates.io at exact `=` versions (ADR 0003's
"Amendment — 2026-08-29"); the `=` is deliberate, since those are unsemvered internals. There are
**no npm or PyPI wrapper packages** (they were removed).

Versioning is **lock-step**: bump all surfaces to the same `X.Y.Z` in one change. The single
source of truth is the workspace `[workspace.package] version` in the root `Cargo.toml`; member
crates inherit it via `version.workspace = true`. `.ai-rulez/config.toml [plugin] version` and
the generated `.claude-plugin/plugin.json` / `.claude-plugin/marketplace.json` /
`.codex-plugin/plugin.json` plugin manifests move with it — the plugin (ADR 0022) is one more
lock-step surface, not a separately tracked version.

## Rules

- Tags MUST be `v<version>` (e.g. `v0.1.0`); `.github/workflows/publish.yaml` triggers on
  `v[0-9]+.*` and builds from the tagged commit, failing the run if the tag and the
  `Cargo.toml` version disagree.
- The release workflow cross-compiles the six platform archives (linux gnu/musl x86_64,
  linux gnu aarch64, macOS x86_64/aarch64, windows-msvc x86_64), verifies all six are present,
  then uploads `sha256sums.txt` and un-drafts the release. The installers and the GitHub Action
  fetch the archive matching the host.
- **Homebrew does not use the prebuilt archives.** `scripts/update-homebrew-formula.sh` emits a
  **source-build** formula pointing at the tag's source tarball (`depends_on "rust"`,
  `system "cargo"`), pushed to `Goldziher/homebrew-tap`; the tap's own auto-bottle pipeline then
  builds bottles and commits a `bottle do` block back. Until bottles land, `brew install` builds
  from source. Do not "fix" the formula to fetch a release archive — the absence of a `bottle do`
  block is the signal the tap's bottler keys off.
- We do not publish to crates.io; the binary is the only distribution surface.
- Commit `Cargo.lock` and pin git-dependency `rev`s (all crates from one monorepo share a `rev`)
  so a tagged build is reproducible.
- `scripts/release-bump.sh <version>` bumps `Cargo.toml` and `.ai-rulez/config.toml [plugin]
  version` together, refreshes `Cargo.lock`, regenerates the plugin outputs, and asserts the
  generated `.claude-plugin/*.json` and `.codex-plugin/plugin.json` manifests carry the new
  version before finishing — never bump one surface without the other.
