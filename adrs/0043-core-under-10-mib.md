# 0043 — The Core Stays Under 10 MiB

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

The released core stays under **10 MiB (10,485,760 bytes)** on `x86_64-unknown-linux-gnu`, the
largest native triple, and CI fails above it.

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

`opt-level = "z"` is the base. A cold run is CPU-bound in the tree-sitter C runtime (the `cc` crate
takes `-Oz` from the package's `opt-level`) and in a few small hot-path crates, so those packages
keep `opt-level = 3` (`tree-sitter`, `poly-cache`, `blake3`, `ignore`, `memchr`, `aho-corasick`,
`regex-automata`). Measurements are in `docs/size-baseline.md`: pure `z` made a cold lint of 573
files about 50-60% more CPU time than the current release; `s` stayed within 5% but is about 2.5 MB
larger and does not fit; `z` with the hot packages at 3 fits and costs about 10-15% cold CPU time.

### Gate

`mise run check:core-size` builds the core with `--profile core` and fails above the ceiling; CI
runs it on Linux, and the release workflow checks the shipped `x86_64-unknown-linux-gnu` binary
(which also embeds the component lock).

## Consequences

- Cold runs on the core cost about 10-15% more CPU time than a thin-LTO `opt-level = 3` build
  (a warm run about 20-30 ms), above the 5% guideline; the ceiling is the harder constraint.
- Two more engines need a component to be used: `uncomment` and `ini`. A core build without the
  lock reports them like any other missing component.
- Any dependency that grows the core past the ceiling fails CI, and the answer is a component, not
  a higher ceiling.

## Alternatives considered

- **`opt-level = "s"` for the whole core.** Meets the 5% speed budget but is 11.3 MB on
  `aarch64-apple-darwin`, about 13 MB on Linux. Rejected.
- **Pure `opt-level = "z"`.** Smallest, but cold runs cost 50-60% more CPU time. Rejected for the
  hot-path packages above.
- **Raising the ceiling.** The ceiling is a decision about the first download, not a measurement.
