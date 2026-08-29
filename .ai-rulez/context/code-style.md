---
priority: high
---

# Code Style

Project-specific conventions baked into context so they ship into every AI tool's config.

## Module layout

- **One concern per file.** A backend lives in `crates/poly-core/src/engines/<tool>.rs`;
  pipeline stages live in their own files (`discover.rs`, `cache.rs`, `runner.rs`,
  `report.rs`, …). Match that shape when adding a new tool area.
- **1000-line cap** on every `*.rs` file, enforced by the `rust-max-lines` hook in `poly.toml`. Refactor
  by extracting helpers, types, or submodules — never by lifting the cap. When an
  `engines/<tool>.rs` grows past it, split per backend concern (e.g. `<tool>/lint.rs`,
  `<tool>/format.rs`, `<tool>/config.rs`).
- Per-backend tests live alongside the pipeline contract in `crates/poly-core/tests/`.

## Performance

- **rayon `par_iter` over discovered files** is the parallelism unit — saturate available
  cores. Never spawn raw threads or `tokio::spawn` in the runner.
- **blake3 content-hash caching** skips unchanged work; the cache key folds in engine name +
  version + resolved config. **When an engine's dependency source changes, `Engine::version()`
  must change with it** — otherwise the cache serves results computed by the old dependency under
  the new one, a stale-cache correctness bug no test run reveals. (`RuffEngine::version()`
  embedded the git rev and now embeds `ruff-0.16.5+…`; ADR 0003's 2026-08-29 amendment.)
- Reuse tree-sitter parsers via a pool — never construct one per file.
- Avoid `.clone()` in inner loops; prefer `&str` / `&[u8]`. Defer ownership to the boundary.

## Dependency policy

- **Pure-Rust, in-process, no subprocess, no system dependency — by default.** Every wrapped
  tool is compiled in as a crate dependency; engines never shell out. **One scoped
  exception:** *native-toolchain backends* (see crate-layout) may invoke a language's canonical
  first-party CLI — `gofmt`, `rustfmt`, `zig fmt`, … — when it is present on the host. The two
  canonical formatters with no viable Rust library, **`rustfmt` and `gofmt`, are default-on when
  present** (ADR 0014 amendment, 2026-06-28) — matching what `cargo fmt`/`gofmt` users already
  expect; the rest (`zig fmt`, and all opt-in lint/format tools) stay **off by default**. In
  every case, when the tool is absent the language falls through to the tree-sitter tier, so the
  zero-dependency guarantee still holds for anyone without the toolchain installed. (`poly
  hooks`/polyhooks is a separate, pre-existing exception, since running foreign hooks inherently
  shells out.)
- **Prefer crates.io, at an exact `=` version; use a pinned git `rev` only when the library we
  need isn't published.** Since 2026-08-29 ruff comes from the registry: `ruff_linter = "=0.16.5"`
  and `ruff_db` / `ruff_formatter` / `ruff_python_ast` / `ruff_python_formatter` /
  `ruff_text_size` = `"=0.0.11"`.
- **The `=` is deliberate.** These upstream internals are private-by-intent APIs their authors do
  not semver. A caret range on such a crate lets `cargo update` walk into unannounced breakage
  between releases — exactly the failure the git `rev` used to prevent. An exact pin keeps that
  property. (Ordinary, publicly-semvered dependencies take normal ranges.)
- **Three git pins remain — `oxc`, `biome`, `rubyfmt`** (`deny.toml`'s `allow-git` is the live
  list). Each is *unpublishable*, not merely unpublished: four of the oxc crates poly uses carry
  `publish = false`, and a partial migration is a hard compile error because cargo treats a
  registry crate and a git crate as distinct instances; biome's published `biome_analyze 0.5.7`
  is a 2024 tarball under the same version number as our 2026 pin; crates.io `rubyfmt` is a
  `0.0.0-wip` name reservation. When several crates come from one monorepo (e.g. all of oxc), pin
  them to the **same `rev`** so their internal versions stay consistent.
- **A git pin records upstream's publishing posture at a moment in time, not a permanent
  property — re-check the pins whenever you touch dependencies.** The check is cheap: query
  `https://crates.io/api/v1/crates/<name>` and compare the published version against what the
  crate declares at our pinned `rev`. A name existing on crates.io is not enough — verify it is
  not a placeholder, a yanked squat, or the same version number over far older code. Retiring a
  pin also means dropping its `deny.toml` `allow-git` entry, and changing the engine's
  `version()` (see Performance). Read ADR 0003's 2026-08-29 amendment before touching this.
- **Do not vendor.** A pinned git dep tracks upstream without a forked copy to maintain. (There
  is no `vendor/` directory. Two documented exceptions predate this — the prek port in
  `crates/poly-hooks` and the mdsf tool-catalog data — both recorded in `ATTRIBUTIONS.md`.)
- We do **not** publish our own crates to crates.io (see release-versioning), so git deps are
  fine — crates.io only forbids them when *publishing*, which we don't do.
- Empirically verify a tool's library API before wiring it — clone a git dep to `/tmp` at the
  exact pinned `rev`, or read the exact published version for a registry dep.
- Pin the exact version or git `rev`, and commit `Cargo.lock`, for reproducible builds.
- `cargo deny` gates licenses (no GPL/AGPL) across the full dependency tree, git deps included.

## Opinionated defaults

- Respect each wrapped tool's own defaults as the base, then apply a thin override layer.
- **Line length 120** everywhere a tool exposes the setting. The base layer is `[defaults]`
  (`line_length = 120`, `line_ending = "lf"`, `final_newline = true`,
  `trim_trailing_whitespace = true`).
- **Always format docstrings** (`docstring-code-format = true`, `docstring-code-line-width = 120`).
- Purely stylistic rules: pick one modern convention or turn the rule off — never bikeshed.
- Layering order: tool default → opinionated override → user `poly.toml`.

## Commits

- **Conventional Commit prefixes** (`feat:`, `fix:`, `perf:`, `chore:`, `refactor:`); enforced
  on `commit-msg` by the gitfluff hook. The body explains *why*, not *what*.
- **Commits are signed.**
