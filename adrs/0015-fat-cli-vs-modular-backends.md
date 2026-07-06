# 0015 — Fat CLI vs. Modular Backends

- Status: Accepted
- Date: 2026-06-29
- Supersedes: the on-demand modular-install direction (closes that exploration)

## Context

poly ships every backend compiled into the binary: a single fat CLI (`poly`) distributed as
prebuilt platform artifacts (ADR 0010), like ruff / oxlint / biome. The release binary is
~70 MB, dominated by a few large
in-process backends (oxc, ruff, mago) and the embedded typos dictionary.

A recurring idea was an **on-demand modular install**: a thin CLI core that fetches, verifies,
and caches signed backend modules at runtime, so users download only what they use. The
appealing precedent is tree-sitter-language-pack, which fetches and caches grammars on demand.

The question: do we make backends modular (fetched/loaded at runtime), and if so, how?

## Decision

**Stay a fat CLI. Do not adopt runtime-modular backends.** All backends remain compiled in,
shipped as prebuilt artifacts.

The grammar-pack analogy does not transfer. tree-sitter grammars are **C libraries with a
stable C ABI**, designed to be `dlopen`'d — which is exactly why tslp can fetch and load them
at runtime. poly's backends are **Rust crates**:

- **No stable Rust ABI.** A `dyn Engine` cannot safely cross a `dlopen` boundary between
  independently compiled artifacts. A runtime-plugin model would require defining a C-ABI
  plugin interface (`#[repr(C)]` + manual vtable, or `abi_stable`/`stabby`) and wrapping every
  backend in that FFI shell — a quarter-scale rewrite of the `Engine` contract (ADR 0001).
- **Arena allocators and shared internal type graphs.** oxc/ruff use arena allocators and pass
  arena-bound types internally; oxc's and ruff's crates are pinned to a single shared `rev` for
  type-graph consistency. Splitting them into independently built modules breaks that and
  forfeits dedup — each module would carry its own copy, likely *increasing* total download.
- **Subprocess plugins would violate ADR 0002** (pure-Rust, no-subprocess), the project's
  foundational guarantee, whose only sanctioned exceptions are native-toolchain backends
  (ADR 0014) and `poly hooks`.

The cost of true modularity is therefore disproportionate to the benefit (binary size), and it
conflicts with ADRs 0001/0002.

**Sanctioned cold-start optimization (no ABI change):** statically link the high-traffic
tree-sitter grammars into the binary and leave the long tail on tslp's dynamic+download path.
tslp already exposes this as a build-time choice (`TSLP_LANGUAGES` + `TSLP_LINK_MODE`), so it is
purely a release-build configuration — no code or architecture change. The exact set (top-N) is
to be chosen by measuring the binary-size vs. cold-start trade-off against the dry-run corpora.

**If binary size ever becomes a hard constraint:** the in-architecture lever is **compile-time
Cargo feature gating** to produce slimmer variants (e.g. a build without mago/oxc), selected at
build time — not runtime fetching. This keeps one codebase and full reproducibility.

## Consequences

Positive:

- One self-contained, reproducible binary; no runtime fetch/verify/cache surface, no signing
  infrastructure, and the zero-network guarantee holds (grammars aside, which already degrade
  gracefully).
- The `Engine` contract (ADR 0001) stays a plain in-process Rust trait — no FFI, no ABI shims.
- Distribution stays exactly the ruff/oxlint model already in place (ADR 0010 + the
  installers, Homebrew, and the `Goldziher/poly` GitHub Action).

Negative / risks:

- Binary size stays large (~70 MB). Mitigated by static-linking only the common grammars and,
  if needed later, feature-gated slim builds. Accepted: disk is cheap; download is one-time.
- Users cannot install "just the Python backend" — they get everything. Accepted as the same
  trade-off ruff/oxlint make.

## Alternatives considered

- **Runtime dynamic backend loading (dylib/cdylib, fetched + signed + cached):** rejected — no
  stable Rust ABI, arena-allocator/type-graph hazards, pinned-rev dedup loss; a rewrite of the
  `Engine` contract for a benefit (size) that doesn't justify it.
- **Out-of-process subprocess plugins:** rejected — violates ADR 0002; reserved exceptions are
  native-toolchain backends and `poly hooks` only.
- **Compile-time feature gating now:** deferred — viable and in-architecture, but unnecessary
  until binary size is a demonstrated problem; revisit if it is.
