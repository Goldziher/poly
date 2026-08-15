# 0025 — Build Path Equivalence and Build Identity Across Distribution Channels

- Status: Proposed
- Date: 2026-08-15

## Context

`poly` reaches users through four build paths, and only one of them is what we test:

1. **`cargo build --release`** from the workspace root — the developer build, and what any
   local verification lane sha-checks.
2. **`cargo install --path crates/poly-cli`** — a developer installing from source.
3. **`cargo build --release --target <triple>`** from the workspace root — `publish.yaml`'s
   build matrix, which produces the archives the `curl | sh` / PowerShell installers and the
   `Goldziher/poly` GitHub Action download. (The Action shells out to this repo's `install.sh`,
   so it is a *consumer* of path 3, not a build path of its own.)
4. **`cargo install --locked --path crates/poly-cli`** — the Homebrew formula emitted by
   `scripts/update-homebrew-formula.sh`, which is a **source build** (the tap bottles it
   afterwards), run against the GitHub source tarball for the tag.

Two builds of the same clean tree were measured as producing different binaries:

    cargo build --release                  -> sha256 8496c57f...   (3/3 forced rebuilds byte-identical)
    cargo install --path crates/poly-cli   -> sha256 8c873853...

Since `cargo build --release` was demonstrated reproducible, the divergence lives in the
install path. The suspected mechanism was **Cargo feature unification**: features resolve per
build graph, so a package built standalone can resolve a different feature set than the same
package built as part of a workspace — which would mean the shipped binary behaves differently
from the tested one, not merely that its bytes differ.

## Investigation

All measurements on commit `4700b3a`, `rustc 1.97.1`, `aarch64-apple-darwin`.

### 1. Feature resolution is identical across every path

Compared with `cargo build --unit-graph -Z unstable-options`, taking the transitive closure of
the `poly` **bin** unit in each graph and diffing package set, resolved features, and resolved
profile per unit:

| Comparison | Units in the `poly` closure | Package-set diff | Feature diff | Profile diff |
|---|---|---|---|---|
| workspace root vs. standalone `poly-cli` | 988 vs. 988 | 0 | **0** | 0 |
| workspace host vs. `--target aarch64-apple-darwin` | 786 vs. 786 (pkg, kind, mode) | 0 | **0** | — |

The workspace graph carries 990 units to the standalone graph's 988; the two extra units belong
to other members and are **not reachable from the `poly` bin**, so they cannot affect it.

The hypothesis is **disproven**, and the reason is structural rather than accidental:
`poly-core` exposes exactly one optional feature, `schemars`, and the only crate that enables it
is `poly-mcp` — which `poly-cli` depends on unconditionally. The feature is therefore on in
*any* graph that contains the `poly` binary, workspace-wide unification or not. There is no
optional-feature surface for the paths to disagree about.

### 2. The byte difference is unlocked dependency re-resolution

`cargo install` **ignores the committed `Cargo.lock` unless `--locked` is passed** and
re-resolves every dependency to the newest semver-compatible version. Reproduced directly:

    cargo build --release                                  -> 1424c47e9e05a3d1...
    cargo install --locked --path crates/poly-cli          -> 1424c47e9e05a3d1...   (byte-identical)
    cargo install --path crates/poly-cli                   -> cee0667467fc62c4...   (differs)

The unlocked run printed exactly what it swapped:

    Locking 739 packages to latest Rust 1.97 compatible versions
       Compiling granit-parser v1.1.0
       Compiling serde-saphyr v1.1.0
       Compiling poly-cli v0.21.5

against the `1.0.1` of both that `Cargo.lock` pins. `serde-saphyr` is the YAML deserializer
behind `poly migrate`'s importers, so this is a behavioural surface, not only a byte surface —
and it widens on its own as upstreams publish, with no change to this repo.

So: **the paths agree on features; they disagree on dependency versions, and only when the lock
is discarded.** Paths 1, 3, and 4 all build from the committed lock (Homebrew's
`std_cargo_args` hard-codes `--locked`). Only the bare `cargo install --path` of path 2 drifts,
which contradicts the dependency policy's "pin the git `rev` and commit `Cargo.lock` for
reproducible builds".

### 3. Homebrew ships a binary that cannot claim its own provenance

`poly-buildinfo`'s `build.rs` derives the build id from `git describe --tags --always` against
the source tree. Homebrew builds from the GitHub **source tarball**, which has no `.git`.
Verified by building `poly-buildinfo` from `git archive HEAD`:

    cargo::rustc-env=POLY_BUILD_ID=
    cargo::rustc-env=POLY_BUILD_COMMIT=

which resolves at runtime to:

| Build | `poly --version` | `cache_identity()` |
|---|---|---|
| CI at the tag (paths 1/3) | `0.21.5 (release build v0.21.5, release)` | `release/0.21.5` |
| Homebrew source tarball (path 4) | `0.21.5 (unknown build unknown, release)` | `unknown/0.21.5/unknown/release/<exe fingerprint>` |

Two consequences, both real:

- `BuildChannel::Release` is documented as "what the installer, Homebrew, and the GitHub
  release ship". Homebrew does not ship it — it ships `Unknown`.
- `cache_identity()` is folded into the result-cache key. A `release/<version>` identity is
  machine-independent *by design*, so every machine's `v0.21.5` shares cache entries. A
  Homebrew-installed poly gets a per-binary identity instead, so it shares a cache with nothing
  and re-does all work after every `brew upgrade`. That is the safe direction to fail, but it
  is not the designed behaviour, and `poly doctor` / the MCP identity block report the binary's
  provenance as `unknown` to anyone triaging a bug against it.

`build.rs` already anticipates exactly this case: *"A packager that builds outside git can
supply the id explicitly by setting `POLY_BUILD_ID` at build time."* The formula generator does
not set it.

### 4. Secondary notes

- `cargo install` **ignores project-level config discovery** and reads only
  `$CARGO_HOME/config.toml`. There is no `.cargo/config.toml` in this repo today, so nothing
  diverges — but adding one (e.g. `RUSTFLAGS`, a target linker) would silently apply to
  `cargo build` and not to `cargo install`.
- `find_git_dir` in `build.rs` requires `.git` to be a *directory*, so inside a git **worktree**
  (where `.git` is a file) it keeps walking up and registers `rerun-if-changed` against the
  **parent** repository's refs. The build id itself stays correct — the `git` CLI resolves
  worktrees — only the rebuild trigger is misattributed.
- `axoupdater` is declared in `[workspace.dependencies]` but no member depends on it, so it is
  never built. Harmless, and unrelated to the divergence.

## Decision

1. **The supported build for a shipped artifact is `cargo build --release` from the workspace
   root**, which is what `publish.yaml` already does for every triple. No change.
2. **Every install-from-source path must pass `--locked`.** Document
   `cargo install --locked --path crates/poly-cli` as *the* source-install command; a bare
   `cargo install --path` builds a poly that is not the poly we tested. Homebrew already
   complies via `std_cargo_args`.
3. **A build outside a git checkout must be given its identity explicitly.** The Homebrew
   formula's `install` block should set `ENV["POLY_BUILD_ID"] = "v#{version}"` before invoking
   cargo, which restores `channel() == Release`, the `release/<version>` cache identity, and an
   honest `poly --version` for every tap user.

Decisions 2 and 3 change release and packaging surfaces (`scripts/update-homebrew-formula.sh`,
the README's install instructions) and are therefore recorded here as **Proposed**, pending a
human decision — not applied unilaterally by the investigation that found them.

## Consequences

**Positive.** The feature-divergence question is settled with a reproducible method
(`--unit-graph`, closure from the `poly` bin, diff package set + features + profile) and does
not need re-investigating. The byte difference has a proven, mundane cause. Adopting 2 and 3
makes every distribution channel build the same dependency set and report the same provenance.

**Negative / risks.** `--locked` means a source install receives no upstream fixes until the
lock is bumped here — the correct trade for a tool whose output must be reproducible, and the
same trade CI already makes. Pinning `POLY_BUILD_ID` in the formula means the tap asserts a
provenance rather than deriving it; if the formula's version and the tag ever disagree, the
binary would claim a release it is not built from. That is bounded by the generator taking the
version as its only argument and by `publish.yaml` verifying tag/version consistency before
anything is published.

**Residual risk, unresolved.** `git describe --tags` is ambiguous when several tags point at one
commit. `update_major_tag` force-moves `v0` onto the release commit *after* the artifacts are
built, so a first publish is unaffected — but a later `workflow_dispatch` re-run of an
incomplete release (the only case that rebuilds, since `release_assets_exist` short-circuits
otherwise) fetches both tags, and `git describe --tags` may then return `v0`, whose
`strip_prefix('v')` is `"0"` rather than the version. The binary would silently be stamped
`dev`. Setting `POLY_BUILD_ID` explicitly in the release build would close this too.

## Alternatives considered

- **Have `poly-cli` re-declare `poly-core`'s optional features.** Defensive against a
  divergence that was measured not to exist, and it would add a second place to keep in sync.
- **Fabricate a release channel when the build id is missing** (assume a tarball build is a
  release). Rejected: it is precisely the lie `build.rs` was written to avoid, and it would let
  an arbitrary local source build claim the shared `release/<version>` cache identity.
- **Fold the resolved dependency set into `cache_identity()`.** Solves nothing the lock does not
  already solve, and would cost cross-machine cache reuse for tagged releases.
