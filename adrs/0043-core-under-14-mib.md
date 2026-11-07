# 0043 — The Core Stays Under 14 MiB

- Status: Accepted
- Date: 2026-11-06
- Amends: ADR 0039
- Related: ADR 0040

## Context

ADR 0039 sized the core at about 9.4 MB, measured before the component host and the alef runtime
were added. v0.33.1 shipped a core of 17.3 MB (`aarch64-apple-darwin`) and 21.1 MB
(`x86_64-unknown-linux-gnu`), stripped: the release profile is thin LTO at `opt-level = 3`, and
nothing gated the size. The core is the first download of every user, so its size is a product
property and needs a gate.

## Decision

The released core stays under **14 MiB (14,680,064 bytes)** on `x86_64-unknown-linux-gnu`, the
largest native triple, and CI fails above it. The first target was 10 MiB; speed was chosen over it
(below).

### Scope of the core

The core is poly's own code plus git-hook functionality: config, discovery, cache, runner, reports,
the CLI, `poly hooks`, `poly commit` with gitfluff, `poly migrate`, `poly rules` and `poly doctor`,
the component host, and the engines poly owns: the tree-sitter generic tier, `quality`,
`polyconfig`, the native-toolchain tier and the catalog and recipe tiers. Anything that wraps an
outside engine is a component. That amends ADR 0039's core set in two places:

- `uncomment` (the third-party `uncomment` crate) leaves the core. It is cross-cutting and off by
  default, so it is its own component, marked opt-in like `typos`: a project that does not set
  `[lint.uncomment] enabled = true` never downloads it. Folding it into `misc` would make every
  Dockerfile, dotenv, HCL, INI or GraphQL user download a comment remover they did not ask for.
- `ini` (`rust-ini`) leaves the core and joins the `misc` component with the other small
  single-language linters.

`quality` stays: it is poly's own code and shares the tree-sitter runtime.

### The `core` Cargo profile

`[profile.core]` inherits `release` with fat LTO, one codegen unit and `strip = true`. `panic`
stays `unwind` because `engine_guard` catches an engine panic. The release builds the core with
`--profile core` and reads it from `target/<triple>/core`; the musl bundled build keeps `release`.

`opt-level = "s"` for the whole core, with no per-package overrides. Cold-run CPU time is what a
user feels, and `s` stays within 5% of the v0.33.1 profile (thin LTO, `opt-level = 3`), where `z`
costs 50-60% more (the tree-sitter C runtime takes `-Oz` from the package's `opt-level`) and `z` with
the hot-path packages raised to 3 still costs 11-16%. `s` does not fit 10 MiB: 11,292,288 bytes on
`aarch64-apple-darwin` (11,076,848 after the later cleanups, measured on this branch), about 13 MB on
Linux. The maintainer chose speed over the 10 MiB target, so the ceiling is 14 MiB. Measurements are
in `docs/size-baseline.md`.

`schemars` stays linked: `poly config schema` and the `schemas.output` host service (reached through
`poly-config` and `poly-host-services`) need the report and config derives. Only the component
protocol's own derives are behind a feature.

### Gate

`mise run check:core-size` builds the core with `--profile core` and fails above the ceiling; CI
runs it on Linux, and the release workflow checks the shipped `x86_64-unknown-linux-gnu` binary
(which also embeds the component lock).

## Consequences

- Cold runs on the core stay within about 5% of a thin-LTO `opt-level = 3` build, at the cost of a
  14 MiB core instead of 10 MiB.
- Two more engines need a component to be used: `uncomment` and `ini`. A core build without the
  lock reports them like any other missing component.
- Any dependency that grows the core past the ceiling fails CI, and the answer is a component, not
  a higher ceiling.

## Alternatives considered

- **`opt-level = "z"` with the hot-path packages (`tree-sitter`, `poly-cache`, `blake3`, `ignore`,
  `memchr`, `aho-corasick`, `regex-automata`) at 3.** Fits 10 MiB (8.8 MB on `aarch64-apple-darwin`,
  10.2 MB on `aarch64-unknown-linux-gnu`) but costs a cold run 11-16% more CPU time. Rejected for speed.
- **Pure `opt-level = "z"`.** Smallest, but cold runs cost 50-60% more CPU time. Rejected.
- **Keeping 10 MiB.** Reachable only with `z`, at the speed cost above. The ceiling follows the chosen
  profile; later growth is answered with a component, not another raise.
