# 0039 — Downloadable Engine Components

- Status: Accepted
- Date: 2026-11-03
- Supersedes: ADR 0015
- Amends: ADR 0002

## Context

poly ships every engine in one binary (ADR 0010, ADR 0015). Today's release binary is 108,359,856
bytes stripped (135.5 MB unstripped); `docs/size-baseline.md` attributes it mostly to a few
engines: sqruff's dialects alone are 20.6 MB of code, and the typos dictionary adds 11.1 MB of
data. Every user downloads all of it, whichever languages their repository contains.

ADR 0015 rejected runtime-modular backends and chose a fat CLI. It gave four arguments:

1. Rust has no stable ABI, so a `dyn Engine` cannot cross a `dlopen` boundary.
2. oxc and ruff use arena allocators and a shared internal type graph, so splitting them forfeits
   dedup and could increase the total download.
3. Subprocess plugins would violate ADR 0002.
4. A fetch/verify/cache surface and signing infrastructure would be a new attack and maintenance
   surface, and the zero-network guarantee would be lost.

It also named the lever to use if size became a hard constraint: compile-time feature slimming.
Epic #48 revisits the decision because the premises changed:

- `xberg-io/alef` (`alef-component-abi`, `alef-component-runtime`) already provides what ADR 0015
  said poly would have to write: a C-ABI component interface, a transport, a download manager,
  a content-addressed cache and signature verification.
- poly can now define the boundary at the message level (bytes in, bytes out) instead of at the
  `Engine` trait, which removes the arena and type-graph problem.
- Measurement (`docs/size-baseline.md`) shows a core under 10 MB is reachable and that the full
  set of components is smaller than today's binary.

Companion ADR 0038 fixes the config and CLI surface (`[engines.<id>]`, `[components]`,
`poly components`, `--offline`). This ADR records the mechanism, the trust model and the size
decisions.

### Decision drivers

- A small first download: most repositories need two or three languages.
- Output must not depend on what is installed on the machine.
- The trust root must be something a user can audit, not a server that can change.
- Engines keep their current semantics; the split must not change any diagnostic or formatted
  byte (bundled and component modes are tested for parity).
- The contract must not assume the implementer is Rust.

## Decision

poly 2.0 is a **small native core that downloads versioned engine components on demand**. The
mechanism is adopted from alef rather than built in poly.

### Adopt alef

alef owns the ABI, the transports, the producer codegen, packaging, signing, the lock, the cache
and the download. poly owns the contract semantics (messages and capabilities), the engine proxy,
presets, the CLI, MCP and config. A gap found in alef is fixed upstream, not worked around in poly
(the epic tracks them as `xberg-io/alef#824`-`#845`).

### Noun: component

The unit that is downloaded is a **component**, as in `rustup component`. A component bundles
engines; the engine stays the unit users configure. The alternatives (toolchain, plugin, pack)
collide with ADR 0014, ADR 0022 and ADR 0029 and are rejected for the reasons in ADR 0038.

### Contract

```rust
trait PolyComponent: Send + Sync {
    fn describe(&self) -> Result<Vec<u8>, ComponentCallError>;
    fn call(&self, header: &[u8], body: &[u8]) -> Result<Vec<u8>, ComponentCallError>;
}
```

- Both methods are fallible, because a proxy that cannot fail panics when a component is
  corrupt or a call is rejected. Both are synchronous: the runner is CPU-bound and rayon-driven
  (ADR 0009).
- File content travels in the borrowed `body`, so the host copies nothing.
- The request vocabulary lives in `poly-component-protocol` (`Init`, `RegisterConfig`, `Lint`,
  `Format`, `LintBatch`, `FormatBatch`, `ProjectCheck`, `SkipReason`, `ProvidesLanguage`,
  `BatchSupport`, `DerivedOptionKeys`, `RunContext`, `FileContext`, `LintInputSalt`,
  `FormatInputSalt`, ...). A coverage test pins it to the `Engine` method set. `Describe` must
  equal the compiled index or the component is rejected.
- Hot-path hooks that decide cache identity (`run_context`, `file_context`, input salts,
  `format_config` resolution) run in the host through `poly-engine-api` host hooks. A stat-fast-path
  or cache hit therefore never loads a component, and the core links no oxc crates (a CI gate).

### Wire format: MessagePack with a published JSON Schema

Messages are MessagePack with **named fields** (`rmp-serde`), carried in dedicated wire DTOs and
never the engine types directly. A JSON Schema generated from the DTOs is published for
non-Rust implementers. postcard was rejected: it is Rust-only, its positional encoding breaks on
`skip_serializing_if`, and it cannot decode `toml::Value`, which engine options use.
`PROTOCOL_VERSION` is exchanged in `Init`; a mismatch is `incompatible`, not undefined behaviour.

### Component kinds

- `engine`: per-file lint and format, plus the project-check capability below.
- `frontend`: the MCP server. It needs a host-services callback table
  (`xberg-io/alef#838`), which is why it is a separate kind.
- `sidecar`: reserved. A component with its own runtime, for example a future `sh` component
  written in Go, would implement the same contract over a process transport
  (`xberg-io/alef#842`). The contract is language-neutral so that adding it changes no message.

### Versioning, lock and artifacts

Components version in **lock-step with poly**. The core embeds `components.lock.json` (URLs,
sha256, size, keys). `required_version` in `poly.toml` pins poly and therefore every component;
there is no project-level component lock (ADR 0038).

Artifacts are **content-addressed by an input hash**: engine crate versions from `Cargo.lock`,
protocol and API source hash, rustc, target and profile. Release CI reuses the previous release's
asset and sha when the inputs are unchanged, so an upgrade re-downloads only changed components.
Garbage collection is age-based, using a last-used stamp.
`ComponentEngine::version()` is `<index version>+c<lock sha256[..16]>`, computed from the embedded
lock without loading anything, so the cache key (ADR 0008) changes exactly when a component does.

### Trust model

The **sha256 embedded in the core is the root of trust.** A downloaded archive is accepted only if
its digest equals the digest compiled into the binary the user already trusts, which makes the
download server and its TLS irrelevant to integrity. Ed25519 signatures are kept for the cases
where the core's lock is not the authority: mirrors, `poly components import` archives and
side-loaded locks. A tampered archive and a wrong contract hash are both rejected (tests in the
epic's verification list).

### Strict download rule

Nothing downloads unless:

1. a lint or format run (CLI, MCP or hooks) discovers files whose engines live in a component
   that is not installed, or
2. config lists components to install, or `poly components install` / MCP `components_install` is
   called explicitly.

`components list`, `status`, `config validate`, `config show` and `doctor` never download.
`--offline` (`POLY_OFFLINE=1`) governs components and every other network path: grammars, catalog
installs, remote `extends` and hook sources.

### Missing component: exit 2

A needed component that is unavailable fails the run with **exit 2** (`on_missing = "fail"`). A
skip is allowed only under `install = "never"`, `--offline` or `--on-missing skip`, and is counted
so `--deny-skips` applies. poly never falls back to the tree-sitter tier for that language:
that would make output differ between machines and quietly lower coverage, the failure ADR 0031
guards against. Required components are resolved in a pre-plan pass, before engine planning calls
`provides_language_*` and `batch_support`; an unavailable one becomes a stub engine carrying its
skip or error reason.

### typos is opt-in

typos ships as its own component and **the default changes to off**
(`[engines.typos] enabled = true`). 11.1 MB of dictionary for a default-on engine would force a
download on every first run. `poly migrate` writes `enabled = true` into v1 configs that relied on
the old default.

### MCP is an optional frontend component

`poly mcp` fetches the `mcp` frontend component on first use. The 11 existing tools are kept; the
lint and format tools become task-optional and open-world, with `components`,
`components_install` and `components_uninstall` added. The core stays free of rmcp, tokio and
schemars (about 1.3 MB at O3).

### Project-check capability

An engine may declare `capabilities.project`. The host sends
`ProjectCheck { root, files, config, environment }`, where `environment` carries the interpreter,
site-packages and Python version resolved by the host, so no subprocess runs inside a component.
It runs in a new runner phase after the per-file tier and is cached on (component sha, config,
sorted file digests). Users are adrlint's tree-wide rules (ADR 0037's `MADR41`-`44`, `ADR50`/`51`,
which a per-file engine cannot own), Python type checking (ty first, pyrefly second) and skif.

### musl ships the full binary

`x86_64-unknown-linux-musl` builds with `--features bundled`: every family linked in, no
components. A statically linked musl process cannot `dlopen` a component. The other five triples
ship the core plus components. Bundled and component modes must produce byte-identical insta
fixtures (the `component-parity` job).

### Size: what is in the core, what is a component

All numbers are from `docs/size-baseline.md`: `x86_64-unknown-linux-gnu`, rustc 1.99.1, stripped.

**Core.** Fat LTO with `opt-level = "z"`: **9,418,728 bytes (9.4 MB)**. The levers measured:

| Profile | Stripped bytes |
| --- | ---: |
| thin LTO, `opt-level = 3` | 15,721,760 |
| fat LTO, `opt-level = 3` | 13,707,792 |
| fat LTO, `opt-level = "s"` | 10,020,400 |
| fat LTO, `opt-level = "z"` | 9,418,728 |

It already contains config, discovery, cache, runner, reports, hooks, gitfluff, the tree-sitter
runtime with tslp grammar fetching, the catalog and native-tool tiers, clap and ureq with rustls.
Marginal cost of the items weighed for it (fat LTO, `z`):

| Item | Delta (bytes) | Decision |
| --- | ---: | --- |
| quality | 141,008 | core (shares the tree-sitter runtime) |
| uncomment | 178,352 | core (same) |
| ini | 40,544 | core |
| astgrep + built-in pack | 814,272 | **own component** (would breach the ceiling) |
| dockerfile, dotenv, hcl, graphql | 154,160 + 252,528 + 215,312 + 565,128 | `misc` component (about 1.2 MB together) |
| ruby | 448,704 | component |
| nix | 271,048 | component |
| php (mago) | 2,729,016 | component |
| biome CSS/GraphQL | 5,480,160 | component |
| python (ruff) | 6,150,312 | component |
| sql (sqruff) | 7,188,376 | component |
| web (oxc, oxfmt, markup_fmt, rumdl, adrlint, malva, yaml, taplo) | 15,810,992 | component |
| typos + dictionary | 11,124,984 | component, opt-in |

With quality, uncomment, ini and the alef runtime the core is estimated at about 10.1 MB, under
10 MiB. The 10 MB figure is a ceiling, not a target; the gate is that `runner_e2e` shows no
regression at `opt-level = "z"` for the core's own hot path (discovery, cache, runner), failing
which the core moves to `"s"` (10.0 MB, at the ceiling). Components keep their own profile (thin
LTO, `-C strip=symbols`).

**Total download.** A component carries its own copy of std, serde, the MessagePack codec and
regex. The baseline assumes about 1 MB of duplication per component until measured end to end:

| Component | Estimated download |
| --- | ---: |
| `web` | 16.8 MB |
| `sql` | 8.2 MB |
| `python` (ruff; ty adds more) | 7.2 MB |
| `biome` | 6.5 MB |
| `php` | 3.7 MB |
| `typos` (opt-in) | 12.1 MB |
| `astgrep` | 1.8 MB |
| `misc` | 2.2 MB |
| `ruby` | 1.4 MB |
| `nix` | 1.3 MB |
| `mcp` | about 2 MB |

`all` is about **63 MB of components plus a 10 MB core, against 108 MB for today's stripped
binary**. A Python-only repository needs the core and `python`, about 17 MB. sqruff is 25% of
today's binary (20.6 MB of dialect code); if `sql` must shrink, its dialects are the target.

**Cold start.** alef verified a library with a full SHA-256 on every start. For a 30 MB library
that costs 24.6 ms +/- 0.6 ms with SHA-NI and roughly four times that without. Today's whole
warm-cache pre-commit run on five files takes 32.2 ms +/- 18.8 ms (`--no-cache`: 60.7 ms), and
`poly --version` takes 6.3 ms, so re-hashing on every start would double a hook run. poly's host
therefore uses alef's verified stamp (`xberg-io/alef#833`, shipped), and a cache hit never loads
a component at all (hot-path hooks run in the host).

### Answers to ADR 0015

ADR 0015 is superseded; its four arguments are answered here.

- **Stable ABI.** alef supplies one: the boundary is a C-ABI cdylib with a fixed
  `describe`/`call` byte interface, not `dyn Engine`. No Rust type, vtable or allocator crosses it,
  so rustc and dependency versions inside a component are irrelevant to the host. The cost ADR 0015
  priced as "a quarter-scale rewrite of the `Engine` contract" is paid once, in alef and the wire
  DTOs, and the in-process `Engine` trait stays as it is: a component-side adapter exposes engines
  through the byte interface and a host-side `ComponentEngine` proxy implements `Engine` over it.
- **Arena allocators and type graph.** The arena-bound types never leave a component, because only
  serialized diagnostics and strings cross the boundary. Each component is built from one cargo
  workspace with one lockfile, so oxc and ruff keep the shared `rev` consistency ADR 0015
  required. Dedup is not lost where it mattered: all oxc consumers (oxc, oxfmt, rumdl, adrlint,
  markup_fmt, malva) live together in `web`, and ruff and ty share `python`. What is duplicated
  is std, serde and regex per component, measured as about 1 MB each, which is why the total for
  `all` (about 73 MB with the core) is still below today's 108 MB rather than above it as ADR 0015
  feared.
- **Zero network.** ADR 0015 kept zero-network as a property of the binary. This ADR keeps it as a
  property of the *run*: no command downloads unless a run needs a missing component or the user
  asks (strict download rule), `--offline` turns every network path into a skip or an error, and
  `bundled` musl and `components import` serve fully offline use. The network is trusted for
  availability only; integrity comes from the sha256 embedded in the core.
- **Dedup.** Covered above for the type graph. The remaining dedup question, a changed engine
  forcing a re-download of everything, is solved by content-addressed artifacts: an unchanged
  component keeps its asset and sha, so an upgrade fetches only what changed.
- **Subprocess plugins.** Not adopted. Components are in-process dynamic libraries; the
  sidecar kind is reserved, not built, and would need its own ADR before it ships.

### Amendment to ADR 0002

ADR 0002 stated that the only sanctioned "downloading something" is precompiled tree-sitter
grammars, because they are **data**. Components are downloaded **executable code**, loaded into
the poly process. ADR 0002 is amended (see its Updated line):

- Downloading executable code is permitted, but only as a component, only under the strict
  download rule, and only when its sha256 matches the digest embedded in the core (or a
  signature verifies, for mirrors, imports and side-loaded locks).
- "No system dependency" is unchanged: a component needs no host toolchain or runtime, and
  `x86_64-unknown-linux-musl` still ships a complete binary with nothing to fetch.
- "No subprocess" is unchanged for engines. Project checks receive their environment from the
  host and run in-process.

## Consequences

**Positive.** The first download falls from 108 MB to a 10 MB core plus the components a repository
uses, 17 MB for a Python-only repository. Upgrades fetch only changed components. The core is
crates.io-only and links no oxc, so its supply chain is auditable. The contract is
language-neutral and has a reserved route for non-Rust engines. Whole-project tools (adrlint's
tree rules, ty) get a first-class phase.

**Negative.** A first run on a new machine needs the network unless the component was installed,
imported or is a musl bundled build, and a run without it exits 2. Two ways to ship (bundled and
components) must stay byte-identical. The build is larger to reason about: a second cargo
workspace for git pins, a content-addressed release pipeline and a lock embedded in the core.
`all` is a larger *total* than the core, and each component carries about 1 MB of duplicated std
and serde. Windows policy tooling (WDAC) can block a freshly downloaded DLL, and pruning a loaded
DLL must defer.

**Risk.**

- *Core over the ceiling.* The 9.4 MB figure excludes quality, uncomment, ini and the alef
  runtime (about 10.1 MB estimated). If the real figure is over, move quality or uncomment out,
  or fall back to ADR 0015's compile-time slim variants.
- *Per-component duplication* is an estimate until the end-to-end component build is measured;
  the `all` total must be re-recorded then.
- *Cold start* depends on the verified stamp and on SHA-NI for any first load.
- *NFS home directories:* file locks are unreliable there; documented, not solved.
- *ty* is beta and moves with the ruff pin.

### Confirmation

- A size gate records the core size and per-component sizes from `scripts/measure-core-size.sh`
  and fails above the ceiling.
- A `cargo tree` check in CI asserts the core contains no `oxc_` crates and no git dependency.
- The `component-parity` job asserts the insta fixtures are byte-identical in bundled and
  component modes.
- Tests assert a tampered archive and a wrong contract hash are rejected, that the cache key
  changes when a component sha changes, and that a `file://` lock installs on first use.
- Offline tests with nothing installed assert exit 2, and a counted skip under
  `--on-missing skip`; `components status`, `config validate`, `config show` and `doctor` make no
  network request.
- The `hyperfine` cold-start run from the baseline is repeated with components loaded and compared
  with the numbers above.

## Alternatives considered

- **Stay a fat CLI (ADR 0015).** Rejected: 108 MB for every user, and the stated arguments no
  longer hold against a message-level C-ABI boundary and measured dedup.
- **Compile-time slim variants only.** Viable, still available as a fallback, but it multiplies
  release artifacts and does not give a user a one-command way to add a language.
- **Subprocess plugins.** Rejected as in ADR 0015; they would also break ADR 0002 for engines.
- **WASM components.** Rejected as in ADR 0002: a runtime of its own, and oxc/ruff/sqruff at
  native speed matter for the per-file hot path.
- **Our own plugin loader instead of alef.** Rejected: ABI, transports, signing, cache and
  download are already built and maintained in alef; poly keeps only the semantics.
- **postcard or JSON on the wire.** postcard rejected above; JSON is larger and slower for file
  bodies, and the JSON Schema covers the non-Rust implementer need.
- **Fall back to the tree-sitter tier when a component is missing.** Rejected: output would
  differ between machines.
- **A project-level component lock.** Rejected: lock-step versioning and `required_version`
  already pin every component.
- **typos default-on with auto-install.** Rejected: 11.1 MB of dictionary on every first run for
  an optional check.
- **Keeping quality, uncomment or astgrep out of the core by default.** astgrep is out (0.8 MB
  would breach the ceiling); quality (0.14 MB) and uncomment (0.18 MB) share the tree-sitter
  runtime and stay in.
