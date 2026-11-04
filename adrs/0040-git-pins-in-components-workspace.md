# 0040 — Git Pins Live in the Components Workspace

- Status: Accepted
- Date: 2026-11-04
- Amends: ADR 0003
- Related: ADR 0039

## Context

ADR 0003 allows pinned git `rev` dependencies when an upstream library is not published, and its
2026-08-29 amendment left three of them: oxc, biome and rubyfmt. They sit in the root workspace, so
the whole tree carries `git+` sources in `Cargo.lock` and `allow-git` entries in `deny.toml`. That
blocks a core that builds from crates.io alone, which ADR 0039 needs: the core stays small and the
engines that need git pins ship as components.

The pins are blocked upstream, not merely unpublished. oxc's `oxc_linter` and the `oxc_formatter*`
family are not published, or are placeholders. biome's published analyzer crates are 2024 code under
current version numbers. The crates.io `rubyfmt` is a `0.0.0-wip` name reservation.

## Decision

Git pins live only in a separate cargo workspace, `components/`, behind poly-owned bridge crates.

- The root `Cargo.lock` contains no `git+` source and the root `deny.toml` has no `allow-git`.
- `components/` has its own `Cargo.lock` and `deny.toml`. Its `allow-git` lists only pins still
  blocked upstream, each with a comment naming the crate that is missing from crates.io.
- Each pinned upstream is wrapped by one bridge crate that exposes poly's own message-level API. The
  root and every other crate depend on the bridge, never on the pinned crate.
- When upstream publishes a usable release, the bridge switches to crates.io at an exact `=`
  version and the `allow-git` entry is removed. A weekly workflow
  (`.github/workflows/crates-io-watch.yaml`) queries crates.io for each blocked crate and fails
  when one gets a real release, so the switch is not forgotten.
- alef builds the second workspace through `source_manifest` and `producer_dir`
  (`xberg-io/alef#866`).

Per pin:

| Pin | Status | Blocked by | Component |
| --- | --- | --- | --- |
| oxc | blocked upstream | `oxc_linter`, `oxc_formatter`, `oxc_formatter_core`, `oxc_formatter_css`/`json`/`markdown`/`toml`/`yaml` not published (`oxc_formatter` is a `0.0.0` placeholder) | web |
| biome | blocked upstream | `biome_analyze`, `biome_css_analyze`, `biome_css_parser`, `biome_graphql_analyze`, `biome_graphql_parser`, `biome_languages`: published `biome_analyze 0.5.7` and `biome_css_parser 0.5.8` are 2024 code | `biome`, its own component |
| rubyfmt | blocked upstream | crates.io `rubyfmt` is a `0.0.0-wip` reservation | `ruby`, its own component |

Everything else in ADR 0003 stands: crates.io first, exact `=` pins for unsemvered internals, no
vendoring, no publishing of poly's crates.

## Consequences

- The core builds and audits from crates.io alone; `cargo deny` in the root has no git exception.
- Two lockfiles and two deny configs to maintain; the watch workflow and parity checks cover drift.
- Bundled builds (musl, and non-musl until the components are downloadable) are produced from
  `components/` rather than the root workspace.
- Retiring a pin touches only a bridge crate, its `Engine::version()` and the components `deny.toml`.

## Alternatives considered

- **Vendor the upstream code.** Rejected: ADR 0003 forbids it, and the copy would drift from
  upstream.
- **Drop the engines that need git pins.** Rejected: every engine is kept; coverage must not
  regress to remove a dependency source.
- **Keep the git pins in the root workspace.** Rejected: it leaves `git+` sources and `allow-git`
  in the root and blocks a crates.io-only core.
