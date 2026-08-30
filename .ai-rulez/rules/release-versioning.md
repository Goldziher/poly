---
priority: high
---

# Release Versioning & Distribution

poly ships **a single binary — `poly`** (driven by one config), distributed the way ruff /
oxlint / biome are: **prebuilt, platform-specific binaries attached to a GitHub release, plus an
installer**. The channels are the `curl | sh` / PowerShell installer (`install.sh` /
`install.ps1`), the GitHub Action (`Goldziher/poly@v0`, `action.yml` — which shells out to the
same `install.sh`), Homebrew (`brew install Goldziher/tap/poly`), Scoop
(`Goldziher/scoop-bucket`, `scoop install poly`), **npm (`@goldziher/polylint`)** and **PyPI
(`polylint`)**. The release artifacts are named `poly-<version>-<triple>.tar.gz` (`.zip` on
Windows), six targets, plus a `sha256sums.txt`; every other channel serves those same archives.
We do **NOT publish to crates.io** — our crates depend on pinned git dependencies (**oxc, biome,
rubyfmt** — see `deny.toml`'s `allow-git`), which crates.io forbids in published crates, and we
have no need for a source-distribution channel. Ruff is **not** a git pin: since 2026-08-29 the
ruff crates come from crates.io at exact `=` versions (ADR 0003's "Amendment — 2026-08-29"); the
`=` is deliberate, since those are unsemvered internals.

**The command is `poly` on every channel.** The npm and PyPI packages are named `polylint`
only because the unscoped `poly` name is taken on both registries; each channel also installs
`polylint` as an *alias* onto the same executable — never a second binary. That alias is part
of the shipped surface on all of them: npm `bin` has two entries, the PyPI distribution has two
console scripts, the Homebrew formula does `bin.install_symlink bin/"poly" => "polylint"`,
`install.sh` writes a relative symlink (`install.ps1` a hard link), and the Scoop manifest's
`bin` array carries both shims.

Versioning is **lock-step**: bump all surfaces to the same `X.Y.Z` in one change. The single
source of truth is the workspace `[workspace.package] version` in the root `Cargo.toml`; member
crates inherit it via `version.workspace = true`. `.ai-rulez/config.toml [plugin] version` and
the generated `.claude-plugin/plugin.json` / `.claude-plugin/marketplace.json` /
`.codex-plugin/plugin.json` plugin manifests move with it — the plugin (ADR 0022) is one more
lock-step surface, not a separately tracked version. So do the wrapper packages:
`npm-package/package.json` **and each of its six exact `optionalDependencies` pins**, the six
`npm-package/platforms/*/package.json` files, `pip-package/pyproject.toml`, and
`pip-package/polylint/__init__.py`.

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
- **The wrapper packages ship the release's own binaries, never a rebuild.** npm publishes six
  per-platform packages (`os`/`cpu`/`libc`-gated, pulled in through `optionalDependencies` —
  no `postinstall` download) and PyPI publishes per-platform wheels carrying the binary as
  package data, plus a `py3-none-any` fallback whose only job is to explain itself on an
  unsupported platform. Both jobs `gh release download` the archives back off the release and
  verify them against `sha256sums.txt` before packaging; a separate build would produce bytes
  the published checksums do not cover.
- **Both registries authenticate with OIDC trusted publishing** — no `NPM_TOKEN`, no PyPI API
  token. The jobs carry `permissions: id-token: write` and upgrade npm (`npm install -g
  npm@latest`) because token-less publishing needs npm >= 11.5.1; without it npm still signs
  provenance but publishes unauthenticated. npm can only configure a trusted publisher for a
  package that *already exists*, so every new package needs one manual publish first (under a
  non-`latest` dist-tag).
- **A publish job must never fail the release.** `publish_npm`, `publish_pypi` and
  `publish_scoop` are `continue-on-error: true`, run after `finalize_release`, and have nothing
  depending on them — a not-yet-configured registry shows as a red job on a green run and
  cannot put the GitHub release back into draft. `publish_homebrew` is the deliberate exception
  and still fails hard on a missing token.
- **Scoop mirrors the Homebrew shape.** `scripts/update-scoop-manifest.sh` rewrites only
  `version`, the 64-bit `url` and the 64-bit `hash` in `Goldziher/scoop-bucket`'s
  `bucket/poly.json`, taking the hash from the release's `sha256sums.txt` rather than
  recomputing it, and asserts that `bin` / `checkver` / `autoupdate` came through untouched —
  losing `bin` is silent, since `scoop install` still succeeds and produces no command. Only
  `x86_64-pc-windows-msvc` exists; do not invent an `arm64` block.
- We do not publish to crates.io; the prebuilt binary is the only thing any channel ships.
- Commit `Cargo.lock` and pin git-dependency `rev`s (all crates from one monorepo share a `rev`)
  so a tagged build is reproducible.
- `scripts/release-bump.sh <version>` bumps `Cargo.toml`, the npm and PyPI manifests, and
  `.ai-rulez/config.toml [plugin] version` together, refreshes `Cargo.lock`, regenerates the
  plugin outputs, and asserts every substitution actually landed — including that the npm
  `optionalDependencies` pins and the `platforms/` directories describe the same package set —
  before finishing. Never bump one surface without the other: a stale pin resolves to the
  previous release's binary under the new version, and it installs cleanly.
