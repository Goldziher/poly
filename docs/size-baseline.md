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
- **In the core:** quality (+0.14 MB, shares the tree-sitter runtime), uncomment (+0.18 MB, same),
  ini (+0.04 MB), gitfluff, tslp, catalog and native-tool tiers, clap, ureq/rustls and the alef
  runtime. Estimated total about 10.1 MB, under 10 MiB.
- **Components:** `web`, `python`, `sql`, `php`, `biome`, `ruby`, `nix`, `typos` (opt-in) and `mcp`
  as planned, plus two changes to the initial cut:
  - `astgrep` becomes its own component (+0.8 MB would put the core over the ceiling);
  - dockerfile, dotenv, hcl and the graphql formatter form a `misc` component (1.2 MB together).
- **sqruff** is 25% of today's binary. Its dialects are the target if `sql` needs to shrink.

Reproduce with `mise run measure:core-size` (about 2-3 minutes per build; each feature build
needs several GB of free disk under `CARGO_TARGET_DIR`).
