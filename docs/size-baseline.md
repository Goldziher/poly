# Size baseline: core, engine families and cold start

Measurements behind the component split (#48, gate #49). All numbers are from
`x86_64-unknown-linux-gnu`, rustc 1.99.1, poly `7f601d4` (0.32.3), on a 4-core Xeon @ 2.30 GHz with
SHA-NI.

## Method

- The engine families are cargo features of `poly-core` (`engine-python`, `engine-php`, …,
  `all-engines` is the default). `poly-cli --no-default-features` builds the **core-only** binary:
  no engine family and no `poly mcp` (`poly-mcp` pulls every engine).
- `mise run measure:core-size` builds the core once, then the core plus one feature at a time,
  with the size profile below, and prints the stripped size and the delta over the core.
- Per-crate attribution of the fat binary comes from `nm -S --size-sort -C` summed by the leading
  path segment of each symbol (`cargo bloat` runs out of memory on the 135 MB binary).

## Today's binary

| Build | Bytes |
| --- | ---: |
| `cargo build --release` (thin LTO, `opt-level = 3`), unstripped | 135,547,160 |
| same, stripped | 108,359,856 |

Largest contributors by code size (thin LTO, O3, of 82.8 MB attributed symbols):

| Crate | MB | Share |
| --- | ---: | ---: |
| `sqruff_lib_dialects` | 20.57 | 24.8% |
| C code (tree-sitter grammars, ring, mimalloc) | 7.72 | 9.3% |
| `core` (monomorphised std) | 6.43 | 7.8% |
| `rumdl_lib` | 3.91 | 4.7% |
| `oxc_linter` | 3.58 | 4.3% |
| `ruff_linter` | 2.84 | 3.4% |
| `poly_core` | 1.47 | 1.8% |
| `biome_js_type_info` | 1.32 | 1.6% |
| `oxc_react_compiler` | 1.30 | 1.6% |
| `sqruff_lib` | 1.10 | 1.3% |

The typos dictionary is data, not code: it adds 11.1 MB to the core on its own (below).

## The released core after ADR 0043

The numbers above predate the component host and the alef runtime. v0.33.1 shipped a core of
17.3 MB (`aarch64-apple-darwin`) and 21.1 MB (`x86_64-unknown-linux-gnu`), stripped: thin LTO at
`opt-level = 3`, nothing gating the size. ADR 0043 sets a 10 MiB (10,485,760 bytes) ceiling on
`x86_64-unknown-linux-gnu`, gated by `mise run check:core-size` in CI and on the shipped binary.

Stripped bytes, `poly-cli --no-default-features --features core`, macOS 27 / aarch64 unless noted
(rustc 1.99.1). Each row is a separate full build:

| Build | Bytes |
| --- | ---: |
| `release` (thin LTO, `opt-level = 3`) | 17,790,712 |
| `core` profile, `opt-level = "s"` | 11,292,288 |
| `core` profile, `opt-level = "z"` | 8,479,904 |
| `core` profile, `z` with `tree-sitter`, `blake3`, `ignore`, `memchr`, `aho-corasick`, `regex-automata`, `poly-cache` at 3, plus `poly-core` at 3 | 10,441,360 |
| **final**: `core` profile, `z` with the packages above except `poly-core` at 3, `uncomment` and `ini` moved to components, schemars off the protocol | **8,840,128** |
| same, `aarch64-unknown-linux-gnu` (Docker `rust:latest`) | **10,171,472** |

`opt-level = "z"` is the only level that fits; `s` misses the ceiling by about 2.5 MB. The linux
aarch64 build is the closest proxy available here for the gated `x86_64-unknown-linux-gnu`
build, and CI is the authority on that one.

### Speed of the opt-level choice

Poly's own repository, Rust files only (573 files), `hyperfine -N`, 15 runs for `--no-cache` and 20
for warm, on a host whose load average was above 90 because other jobs ran on it, so wall times are
noise and CPU time (the minimum over the runs) is the figure to compare. `poly lint --workspace
never --skip astgrep .` and `poly fmt .` with `[fmt.rust.rustfmt] enabled = false` so the run
measures poly, not a rustfmt subprocess:

| Build | lint cold CPU | fmt cold CPU | lint warm CPU | fmt warm CPU |
| --- | ---: | ---: | ---: | ---: |
| thin LTO, `opt-level = 3` (v0.33.1 profile) | 1,131 ms | 1,166 ms | 72 ms | 168 ms |
| fat LTO, `s` | 1,144 ms (+1%) | 1,229 ms (+5%) | 102 ms | 168 ms |
| fat LTO, `z` | 1,822 ms (+61%) | 1,758 ms (+51%) | 100 ms | 169 ms |
| fat LTO, `z` with the hot packages at 3 | 1,255 ms (+11%) | 1,349 ms (+16%) | 91 ms | 199 ms |

`cargo bench -p poly-core --no-default-features --features core` (`generic_formatter`,
`runner_e2e`, thin LTO, `CARGO_PROFILE_BENCH_OPT_LEVEL` set to the level; `benches/README.md`
baselines come from other hardware and were not comparable):

| Bench | 3 | `s` | `z` |
| --- | ---: | ---: | ---: |
| `generic_formatter/format_rust` | 2.62 ms | 2.64 ms | 3.81 ms |
| `generic_formatter/parse_only_rust` | 1.70 ms | 1.77 ms | 2.37 ms |
| `runner_e2e/format_64_rust_files_dry_run` | 2.90 s (loaded) | 0.74 s | 1.12 s |

`s` is within the 5% budget everywhere but does not fit under 10 MiB, so the shipped profile is `z`
with the hot-path packages raised, which costs a cold run about 10-15% CPU time and a warm
run 20-30 ms. That is above the 5% guideline; the ceiling is the harder constraint.

### Where the bytes are

`llvm-nm -n` address gaps by leading crate of the v0 symbol, `opt-level = "s"` build with the final
Cargo.toml, 11.17 MB attributed: C and unattributed 2.2 MB, `core` 1.1, `poly_cli` 0.60, `toml`
0.58, `regex_automata` 0.55, `poly_core` 0.49, `alloc` 0.45, `log` 0.32, `rustls` 0.26,
`poly_config` 0.24, `serde_json` 0.24, `schemars` 0.23, `tracing_subscriber` 0.19,
`serde_saphyr` + `granit_parser` 0.33, `poly_hooks` 0.17, `poly_workspace` 0.16, `rmp_serde`
0.16, `clap_builder` 0.15, `ureq` 0.12. schemars stays because `poly config schema` and the
`schemas.output` host service (the `mcp` component asks for it) need the report and config
derives; only the component protocol's own derives were gated. tree-sitter-language-pack's
`download` feature is how the core fetches grammars and shares `ureq` with the component
host, so it is not duplicated.

### The musl bundled binary

The release builds `x86_64-unknown-linux-musl` from the `components/` workspace
(`poly-bundled`) with the same thin-LTO release profile as the 108 MB baseline, so the 136 MB is not
a profile mistake. A bundled build on macOS (stripped 115.9 MB, thin LTO) attributes
`sqruff_lib_dialects` 14.8 MB, `ty_python_semantic` 6.2 MB, `salsa` 3.3 MB (ty's incremental
engine), `oxc_linter` 3.3, `rumdl_lib` 3.2, `ruff_linter` 2.4, `biome_*` several: the type
checker (`python-types`, ty) and biome's analyzers did not exist when the 108 MB baseline was
taken, and they account for the growth.

## Core-only binary and release-profile levers

`panic = "unwind"` is kept in every variant (`engine_guard` needs it).

| Profile | Stripped bytes |
| --- | ---: |
| thin LTO, `opt-level = 3`, 1 codegen unit | 15,721,760 |
| fat LTO, `opt-level = 3` | 13,707,792 |
| fat LTO, `opt-level = "s"` | 10,020,400 |
| fat LTO, `opt-level = "z"` | **9,418,728** |

The core-only binary already contains config, discovery, cache, runner, reports, hooks, gitfluff,
the tree-sitter runtime with tslp grammar fetching, the catalog and native-tool tiers, clap and
clap_complete, and ureq with rustls. Their individual cost (thin LTO, O3 attribution) is
`rustls` + `ring` + `webpki` 0.61 MB, `clap_builder` 0.27 MB, `ureq` 0.20 MB, `gitfluff` 0.14 MB,
`tree_sitter_language_pack` 0.13 MB plus about 1 MB of C runtime.

## Marginal size of each item over the core

Size profile (fat LTO, `opt-level = "z"`, one codegen unit, stripped).

| Feature | Engines | Delta (bytes) |
| --- | --- | ---: |
| `engine-quality` | quality | 141,008 |
| `engine-uncomment` | uncomment | 178,352 |
| `engine-astgrep` | ast-grep + built-in pack | 814,272 |
| `engine-typos` | typos + typos-dict | 11,124,984 |
| `engine-ini` | ini | 40,544 |
| `engine-dockerfile` | dockerfile | 154,160 |
| `engine-hcl` | hcl | 215,312 |
| `engine-dotenv` | dotenv | 252,528 |
| `engine-graphql` | pretty_graphql | 565,128 |
| `engine-nix` | alejandra | 271,048 |
| `engine-ruby` (now built from `components/`) | rubyfmt | 448,704 |
| `engine-php` | mago | 2,729,016 |
| `engine-biome` | biome CSS/GraphQL lint | 5,480,160 |
| `engine-python` | ruff | 6,150,312 |
| `engine-sql` | sqruff | 7,188,376 |
| `engine-web` (now built from `components/`) | oxc, oxfmt, markup_fmt, rumdl, adrlint, malva, yaml, taplo | 15,810,992 |

MCP (rmcp + tokio + schemars + `poly_mcp`) cannot be built alone yet because `poly-mcp` depends
on every engine; its own code is about 1.3 MB at O3 (`rmcp` 0.59, `poly_mcp` 0.27, `schemars`
0.23, `tokio` 0.19).

## Component estimates

A component cdylib carries its own copy of std, serde, the MessagePack codec and regex. Until #50
measures it, assume about 1 MB of duplication per component on top of the deltas above.

| Component | Estimated download |
| --- | ---: |
| `web` | 16.8 MB |
| `sql` | 8.2 MB |
| `python` (ruff; ty adds more) | 7.2 MB |
| `biome` | 6.5 MB |
| `php` | 3.7 MB |
| `typos` (opt-in) | 12.1 MB |
| `astgrep` | 1.8 MB |
| `misc` (dockerfile, dotenv, hcl, graphql) | 2.2 MB |
| `ruby` | 1.4 MB |
| `nix` | 1.3 MB |
| `mcp` | about 2 MB |

`all` totals about 63 MB of components plus a 10 MB core, against 108 MB for today's single
stripped binary. The typical project installs much less: a Python-only repository needs the core
and `python`, about 17 MB.

## Cold start

`hyperfine -N --warmup 3 --runs 30` against today's fat binary (thin LTO, O3, unstripped). The
commands are as measured on 0.32.3; since then `fmt --check` is `poly fmt` and `--no-workspace` is
`--workspace never`:

| Command | Mean | ± |
| --- | ---: | ---: |
| `poly --version` | 6.3 ms | 0.9 ms |
| `poly fmt --check one.ts` | 12.6 ms | 7.4 ms |
| `poly fmt --check --no-cache one.ts` | 9.5 ms | 2.1 ms |
| `poly lint --no-workspace` on 5 files (py, sql, md, toml, ts), warm cache | 32.2 ms | 18.8 ms |
| same, `--no-cache` | 60.7 ms | 25.2 ms |

Verifying a 30 MB library the way alef did on every start (full SHA-256) costs 24.6 ms ± 0.6 ms
with SHA-NI, roughly four times that without it. That is comparable to a whole warm pre-commit run,
so the verified stamp (xberg-io/alef#833, shipped) must be on in poly's host.

## Decisions

- **The core fits under 10 MB** with fat LTO and `opt-level = "z"` (9.4 MB). `opt-level = "s"`
  lands at the ceiling (10.0 MB). The core's own hot path (discovery, cache, runner) must be
  benchmarked with `runner_e2e` before `z` is final; components keep their own profile.
- **In the core:** quality (+0.14 MB, shares the tree-sitter runtime), gitfluff, tslp, catalog and
  native-tool tiers, clap, ureq/rustls and the alef runtime. `uncomment` and `ini` moved to
  components (ADR 0043); the gated size is in "The released core after ADR 0043".
- **Components:** `web`, `python`, `sql`, `php`, `biome`, `ruby`, `nix`, `typos` (opt-in) and `mcp`
  as planned, plus two changes to the initial cut:
  - `astgrep` becomes its own component (+0.8 MB would put the core over the ceiling);
  - dockerfile, dotenv, hcl and the graphql formatter form a `misc` component (1.2 MB together).
- **sqruff** is 25% of today's binary. Its dialects are the target if `sql` needs to shrink.

Reproduce with `mise run measure:core-size` (about 2-3 minutes per build; each feature build
needs several GB of free disk under `CARGO_TARGET_DIR`).
