# 0003 — Dependency Policy: Wrap First, Vendor Only When Forced

- Status: Accepted
- Date: 2026-06-26
- Updated: 2026-06-28 (pivot to pinned git deps, no crates.io publish, new vendoring
  exceptions)
- Updated: 2026-08-29 (ruff is no longer a git pin — see "Amendment — 2026-08-29" below)

## Context

Given the no-subprocess constraint (ADR 0002), every backend tool must be reached through
Rust crates. Upstream crates vary: some expose clean library APIs, some bury the useful
logic behind a binary `main` and don't externalize a usable entry point, and some are
fast-moving. We need one consistent rule for how we depend on them.

## Decision

A strict ordering:

1. **Prefer published crates on crates.io.** If an upstream crate (e.g. `taplo`,
   `sqruff_lib`, `rumdl_lib`) externalizes the API we need, depend on it directly and
   write a thin `Engine` adapter (ADR 0004).
2. **Use pinned git `rev` for monorepo internals.** When a tool (ruff, oxc) ships useful
   logic only in its monorepo and not as a published crate, depend on the GitHub repo
   pinned to a specific `rev` (commit) for reproducibility. When multiple crates come
   from one monorepo (e.g. all of oxc), pin them to the **same `rev`** to keep internal
   versions consistent.
3. **No version vendoring in a `vendor/` directory.** Do not maintain a forked copy of
   upstream source. Pinned git deps track upstream without duplication and are fine since
   we do not publish our own crates to crates.io (see below).
4. **Two documented vendoring exceptions (see `ATTRIBUTIONS.md`):**
   - **prek (derived & vendored into `crates/poly-hooks`):** git-hook execution primitives,
     ported to sync/rayon form. Vendored copy `crates/polyhooks` retained during
     migration, removed once inlined.
   - **mdsf tool-catalog data:** tool-definition JSON (tool → binary → argv → stdin →
     languages) and fixtures, vendored to populate the built-in tool registry without a
     crate dependency.

**Distribution note:** poly ships **prebuilt, platform-specific binaries** attached to a
GitHub release (see ADR 0010), plus installers (`curl | sh` and PowerShell), a Homebrew
formula, and the `Goldziher/poly` GitHub Action. We do **not** publish crates to crates.io;
pinned git deps are only possible because we distribute binaries.

Each backend begins with an **empirical API check**: clone the crate to `/tmp` at the
exact `rev`, confirm the library API, then wrap or decide on a git dep.

A `cargo deny` license gate guards the full dependency tree (crates.io + git deps and
their transitive deps): no GPL/AGPL. All vendored sources are recorded in `ATTRIBUTIONS.md`
with their license and copyright.

## Consequences

Positive:

- Published crates get upstream bug fixes automatically.
- Pinned git `rev` provides reproducible, auditable dependencies without maintaining a
  fork. Commits pinned in `Cargo.toml` are easy to review and track.
- Binary distribution means git-rev dependencies are fine — they're unavailable in
  crates.io only because crates.io forbids publishing with them.
- `Cargo.lock` commits make builds reproducible; `cargo deny` gates licenses.
- Bounded vendoring (two exceptions, both documented in `ATTRIBUTIONS.md`) keeps the
  maintenance surface minimal.

Negative / risks:

- Pinned git `rev`s mean we're not automatically advanced by `cargo update`; upstream
  changes must be reviewed and explicitly pulled (a feature, not a bug, for this use case).
- The prek port into `poly-hooks` requires ongoing maintenance as prek evolves.
- The mdsf catalog data must be kept in sync if we want new tools; updates are manual.

## Alternatives considered

- **Vendor everything for stability:** rejected — turns the project into a permanent fork
  of a dozen tools; unmaintainable.
- **Unpin all git deps / use semver ranges:** rejected — the resulting churn and
  non-determinism is unacceptable for a dev tool that must be reproducible across a
  team's development and CI environments. Pinning is a strength for tools, not a
  weakness.
- **Publish crates to crates.io:** rejected — it forces unpinning, which then forces
  vendoring; binary distribution is the right boundary.

## Amendment — 2026-08-29 (ruff migrated to crates.io)

Point 2 of the Decision names **ruff** as an example of a tool that ships its useful logic only in
a monorepo. That is no longer true, and the strict ordering did its job: the moment a published
crate became available, the git pin lost its justification.

Astral began publishing the ruff workspace to crates.io on **2026-06-23** and has published every
weekly release since — eleven releases, zero yanks — as an automated step of the release pipeline
(`scripts/publish-crates.py` enumerates workspace members via `cargo metadata` and skips only
`publish = false` packages; none of the crates poly uses carries that key). poly now depends on
the registry:

```toml
ruff_linter = "=0.16.5"
ruff_db = "=0.0.11"          # and ruff_formatter, ruff_python_ast,
                             # ruff_python_formatter, ruff_text_size
```

Three consequences worth recording:

- **The `=` is deliberate, not cargo pedantry.** These are private-by-intent internals that ruff
  does not treat as a public API and does not semver. A caret range on `0.16.x` would let
  `cargo update` walk into unannounced breakage between releases — exactly the failure the git
  `rev` prevented. An exact pin keeps that property.
- **`Engine::version()` had to change with the dependency.** `RuffEngine::version()` embedded the
  git rev (`git-ruff:700421c+…`) and now embeds `ruff-0.16.5+…`. Had it not changed, the
  content-hash cache (ADR 0008) would have served results computed by the old rev under the new
  dependency — a stale-cache correctness bug that no test run would reveal.
- **`deny.toml`'s `allow-git` shrinks with each retired pin.** `https://github.com/astral-sh/ruff`
  was removed. Leaving it would be a permission granted to nothing — the same dead-configuration
  class ADR 0016's amendment describes.

Three `[workspace.dependencies]` entries (`ruff_diagnostics`, `ruff_python_parser`,
`ruff_source_file`) were referenced by no member manifest and no source file, and were deleted.
`cargo machete` does not catch unused *workspace* dependency keys, which is why they survived.

Migrating cost 42 upstream commits — the pin sat that far past the `0.16.5` tag. The diff across
the nine crates is 21 files of rule bodies, snapshots and two `generate.py`, touching **no API
poly calls**; the next weekly release restores them.

### The other three pins stay, and are not the same case

- **oxc** — four of the seven crates poly uses (`oxc_formatter`, `oxc_formatter_core`,
  `oxc_formatter_json`, `oxc_linter`) carry `publish = false` and are permanently unpublishable.
  A *partial* migration is a hard compile error, not merely poor practice: `oxc_formatter::format`
  takes an `oxc_allocator::Allocator` and an `oxc_span::SourceType`, and cargo treats a registry
  crate and a git crate as distinct instances — the registry `SourceType` would not typecheck
  against the git `oxc_formatter`'s own. That is the same-`rev` rule in point 2, and it binds the
  whole monorepo together. (The `oxc_formatter` name on crates.io is an abandoned 2023 crate, all
  eight versions yanked, unrelated to today's oxfmt.)
- **biome** — half the crates poly uses are unpublished, and the published half is a version-skew
  trap: `biome_analyze` declares `0.5.7` both on crates.io and at our pinned rev, but the
  published tarball's `.cargo_vcs_info.json` names a commit from **2024-03-12** against a pin from
  2026-08-29. Cargo would accept `"0.5.7"` without complaint and hand us two-and-a-half-year-old
  code under an identical version number. Biome's publish workflow is `workflow_dispatch`-only and
  was last run in Dec 2024.
- **rubyfmt** — the crates.io `rubyfmt` is a name reservation: one version `0.0.0-wip`, 1,549
  bytes, whose entire `src/main.rs` prints `Hello, world!`. The real library is `librubyfmt/`
  (crate name `rubyfmt`, version 0.14.1) and has never been published.

**The standing rule this adds:** a git pin records upstream's *publishing posture at a moment in
time*, not a permanent property. Re-check the pins when touching dependencies. The check is cheap
— query `https://crates.io/api/v1/crates/<name>` and compare the published version against the
version the crate declares at our pinned rev — and a name existing on crates.io is not enough:
verify it is not a placeholder, a yanked squat, or the same version number over far older code.

## Amendment — 2026-09-30 (dependency refresh for v0.28.1)

`cargo upgrade --incompatible`, plus advancing the three git pins to their remotes' current HEADs,
moved the git-backed dependencies forward and pulled the ruff pins with them.

- **ruff moved `0.16.5`/`0.0.11` → `0.16.9`/`0.0.15`, as one set.** The bump was forced, not
  cosmetic: biome at its new HEAD depends on `salsa ^0.28.5`, while `ruff_db 0.0.11` resolves
  `salsa` to `0.28.2`, so the two could not coexist in one graph. `ruff_db 0.0.15` takes
  `salsa ^0.28.5`, and `RuffEngine::version()` moved to `ruff-0.16.9+…`. The `0.16.x` line is the
  same weekly series; the diff across the crates touched no API poly calls.
- **The git pins advanced to the remotes' HEADs:** biome `99c7049` → `f5af193`, oxc `827fdbd` →
  `7d28a66`, rubyfmt `5185d3b` → `094cdc8`. Every engine whose output they feed had its
  `version()` bumped with it — `biome_css`, `biome_graphql`, `oxc`, `markup_fmt` (embedded oxc) and
  `rubyfmt` — along with `mago` (1.50), `quality` and `treesitter`.
- **The three-way tree-sitter hold lifted.** `uncomment 3.10.2`, `ast-grep-core 0.45.3` and
  `tree-sitter-language-pack 1.20` all now take `tree-sitter ^0.27`, so `tree-sitter` moved from
  0.26 to 0.27 and the `ast-grep`/`uncomment` pins with it. `uncomment`'s word-tag preservation is
  now case-insensitive as a *substring* match, so a comment containing `bug` — including inside
  `debug` — is preserved by its default ignores; pinned by a test in
  `crates/poly-core/tests/uncomment.rs`.

## Amendment — 2026-10-03 (dependency refresh for v0.28.2)

The pins were re-checked against their remotes' HEADs and the registry refresh the user staged was
carried to its engine `version()` strings. One pin had moved; biome and oxc advanced, rubyfmt was
already at head.

- **The git pins advanced:** biome `f5af193` → `dd96b11`, oxc `7d28a66` → `4dce2c2`; rubyfmt
  `094cdc8` was already the remote's HEAD, so it did not move. The biome workspace crates moved
  `0.5.7` → `0.7.0`; the oxc crates poly calls were version-stable (`oxc_formatter 0.71.0`,
  `oxc_parser 0.152.0`), so only the rev changed there.
- **`mago` moved `1.50` → `1.51`** in the staged registry refresh. Its engine `version()` embeds
  the crate version, so it moved to `mago-1.51.0+advisory-sev1` with it — the staged `Cargo.toml`
  bump alone would have left cached results keyed by the old string.
- **Every engine whose output a moved pin feeds had its `version()` bumped:** `biome_css`,
  `biome_graphql`, `oxc` and `markup_fmt` (embedded oxc) carry the new short rev; `rubyfmt` is
  unchanged because its pin did not move. The `version_audit` guard
  (`crates/poly-core/tests/version_audit.rs`) passes against the refreshed `Cargo.lock`.
- **The re-check held:** biome's crates.io `biome_analyze` is still `0.5.7` from **2024-03-12** —
  now two minor versions behind the pinned `0.7.0` git tree; crates.io `oxc_formatter` is the
  abandoned `0.0.0` from 2023-12-08; crates.io `rubyfmt` is still the `0.0.0-wip` name
  reservation. All three stay pinned on a git `rev`.

## Amendment — 2026-10-11 (dependency refresh for v0.29.0)

The three git pins were advanced to their remotes' HEADs and the publishing-posture re-check was
run again. **One stale-cache regression the previous `chore: updated deps` commit left on `main`
was found and fixed as a rider** — the refresh is the reason the `version_audit` guard was
consulted, and it was red.

- **Two git pins advanced, one could not:** oxc `4dce2c2` → `98573d2` and rubyfmt `094cdc8` →
  `f765808`. The moving oxc rev carried its workspace versions with it — `oxc_formatter*` `0.71.0`
  → `0.72.0`, `oxc_linter` `1.86.0` → `1.87.0`, the shared
  `oxc_allocator`/`oxc_span`/`oxc_diagnostics`/… `0.152.0` → `0.153.0` — and pulled four new
  transitive crates (`bitvec`, `funty`, `radium`, `tap`, `wyz`). No board API poly calls changed;
  `cargo check --workspace --all-targets` and the oxc/rubyfmt/markup_fmt fixtures pass against the
  new revs unchanged.
- **The biome pin stays at `dd96b11` — advancing it breaks the Windows build, and the cause is
  upstream in cargo.** biome's HEAD (`e26a76b`) added a test fixture whose relative path is **205
  characters** long:
  `crates/biome_json_analyze/tests/…/noUntrustedLicenses/ignore_deprecated/package.expected.json/Users/ema/www/biome/…/package.json.snap`
  (a snapshot filename that is itself a `/`-joined path). Cargo checks a git dependency out under
  `C:\Users\<runner>\.cargo\git\checkouts\biome-<hash>\<rev>\`, a **73-character** prefix, so the
  checkout path is **278 characters** — past Windows' 260-char `MAX_PATH` — and the build fails at
  `failed to load source for dependency biome_analyze`. The previous `dd96b11` fixture tops out at
  184 relative chars (257 total); oxc `98573d2` is 187 + 71 = 258, rubyfmt 80. Cargo's own
  maintainers confirmed in [rust-lang/cargo#7420] that `[net] git-fetch-with-cli` does **not** help:
  cargo still extracts the repo with libgit2, which ignores `core.longpaths` and the OS long-path
  registry setting ([libgit2/libgit2#3053], open since 2015). No project-side fix exists, so the pin
  holds at the last biome rev whose fixtures fit, and this is re-checked when biome is next touched.
- **Every engine whose output a moved pin feeds had its `version()` bumped:** `oxc` and `markup_fmt`
  (embedded oxc) carry `98573d2`; `rubyfmt` moved to `f765808`. `biome_css` and `biome_graphql`
  stay at `rev:dd96b11` with their pin.

[rust-lang/cargo#7420]: https://github.com/rust-lang/cargo/issues/7420
[libgit2/libgit2#3053]: https://github.com/libgit2/libgit2/issues/3053

- **The registry re-check held, with one new fact.** crates.io `oxc_formatter` is still the
  abandoned, fully-yanked 2023 `0.0.0`; `oxc_formatter_core`, `oxc_formatter_json` and
  `oxc_linter` are still unpublished, so the same-`rev` rule binds the oxc monorepo even though
  the *shared* crates (`oxc_allocator`, `oxc_span`, `oxc_diagnostics`) are now published weekly at
  `0.153.0` — a partial migration is still a hard compile error against the git `oxc_formatter`.
  Biome's `biome_css_analyze`/`biome_graphql_analyze`/`biome_languages` remain unpublished and
  `biome_analyze` is still the **2024-03-12** `0.5.7` tarball. crates.io `rubyfmt` is still the
  `0.0.0-wip` reservation. All three stay pinned on a git `rev`.
- **Stale `version()` strings from `b693941` (`chore: updated deps`) were fixed.** That commit
  bumped `sqruff-lib` `0.40` → `0.41`, `ast-grep-core` `0.45.3` → `0.50.0` and
  `tree-sitter-language-pack` `1.20` → `1.21.1` in `Cargo.toml`/`Cargo.lock` without touching the
  engine strings, so `version_audit` failed for `sqruff` (`0.40.0`), `astgrep`
  (`ast-grep-core-0.45.3`), `treesitter` (`tslp1.20.0`) and `quality` (`tslp1.20.0`). Each now
  embeds the resolved version — `0.41.0`, `ast-grep-core-0.50.0+tslp1.21.1`, `tslp1.21.1`,
  `tslp1.21.1` — which is what the guard exists to force: a stale string would have served
  results computed by the old crates under the new ones.

## Amendment — 2026-10-11 (ruff moved to the 0.17 line)

With the rest of the tree already current, the only crates behind their latest were the six
`=`-pinned ruff crates. They moved as **one set**, since the policy requires it: `ruff_linter
=0.16.9` → `=0.17.0`, and `ruff_db` / `ruff_formatter` / `ruff_python_ast` /
`ruff_python_formatter` / `ruff_text_size` `=0.0.15` → `=0.0.17`. `0.17.0` is the next unsemvered
weekly line, not a patch on `0.16.x`, so this is exactly the case the `=` exists for.

The API poly calls did not change: `cargo check --workspace --all-targets` is clean, and the
`ruff_engine` and full `poly-core` suites pass with no fixture drift. `RuffEngine::version()`
moved to `ruff-0.17.0+…` with it, because the content-hash cache key must invalidate on the
dependency, not merely on the rev label (ADR 0008). Whatever `ruff-0.16.9` cached is now ignored.
`version_audit` confirms the string tracks the resolved `ruff_linter` version.

`cargo upgrade --dry-run --incompatible` reports nothing else behind: every remaining crates.io
dependency already resolves to its latest compatible version, and the three git pins are at their
HEADs (biome held at `dd96b11` for the Windows reason recorded above).

## Update 2026-10-27: the biome pin moved to HEAD and the Windows build is accepted as broken

The biome, oxc and rubyfmt pins moved to their HEADs (`e2e5a36`, `e2be72d`, `884b9fd`) as part of a
full dependency refresh. This supersedes the "biome stays at `dd96b11`" decision above. biome's
fixture path is still past Windows' `MAX_PATH` once cargo's checkout prefix is added, so Windows
builds that resolve the biome git dependency fail until upstream shortens the path or cargo/libgit2
supports long paths. The maintainers chose that over holding the pin; the Windows CI job is expected
to be red and is not a regression to chase. Revisit when biome next touches that fixture or when
[libgit2/libgit2#3053] closes.

## Amendment — 2026-10-28 (the Windows build works again)

Windows CI sets `CARGO_HOME=D:\c` and `core.longpaths`, which shortens the checkout prefix from about
75 to about 30 characters; the longest stray biome snapshot path (205 characters) then fits in
`MAX_PATH`. The Windows jobs are green again and no longer `continue-on-error`. The stray files are
reported upstream as biomejs/biome#12725. Local Windows builds need the same short `CARGO_HOME`.

## Amendment — 2026-10-27 (`dockerfile-parser` is vendored)

`dockerfile-parser` is the one backend crate that is neither a registry dependency nor a git pin: it
lives in the workspace as `crates/dockerfile-parser`. Its upstream, HewlettPackard/dockerfile-parser-rs,
is archived and its last release (0.9.0) pins `snafu` 0.6, `lazy_static` and edition 2018, so waiting
for a new release is not an option and a git pin would freeze unfixed parser bugs. The crate was
copied from upstream commit `08f37891c84d4162daa8d05918e1fdd8f1528624` with its MIT license and
copyright notice (`crates/dockerfile-parser/LICENSE`, `ATTRIBUTIONS.md`), is `publish = false`, and
is maintained in-tree. The parser runs in-process, so panics and stack overflows are poly bugs; the
crate carries property tests for both.

The `version_audit` guard still applies: the lock entry is a path package with no registry source,
so the engine's `version()` embeds the crate's own version (`0.10.0`) plus a `vendoredN` marker that
is bumped whenever the vendored parser's behaviour changes.

`psm` remains at 0.1.24. pest 2.9.5 and `pest_stack_guard` 1.0.0 are both the latest releases and
`pest_stack_guard` still requires `psm =0.1.24` on the supported targets. Disabling pest's default
features would drop the pin but also the stack guard, which is what converts deeply nested input
into a parse error rather than a stack overflow, so the pin is kept.
