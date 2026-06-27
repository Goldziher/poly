# polylint

[![CI](https://github.com/Goldziher/polylint/actions/workflows/ci.yml/badge.svg)](https://github.com/Goldziher/polylint/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](LICENSE)

**One linter and one formatter for every language — pure Rust, in-process, zero system dependencies.**

`poly` is a single CLI with subcommands `poly lint` and `poly fmt` that replace your entire
per-language tool stack — `ruff` + `oxlint` + `oxfmt` + `taplo` + `rumdl` + `shfmt` +
`clang-format` + … and all of their system dependencies (Python, Node, Go, a JVM, …) — with
**one binary and one config file**. Alias binaries `polylint` and `polyfmt` preserve the
crates.io names.

Everything runs **in-process, in pure Rust**. There are no subprocesses and nothing to install
on the host: where a high-quality Rust crate exists for a language it is wrapped directly, and
everything else is covered by a generic tree-sitter–driven formatter whose grammars are fetched
on demand. Install two binaries, drop in one `polylint.toml`, and lint/format the whole repo.

> **Status: active development.** The foundation and the backend waves are in place: the Cargo
> workspace, the `Engine` trait + static registry, TOML config + opinionated defaults, a blake3
> content-hash cache, an `ignore`-based file walker, a rayon-parallel runner, three output
> formats (`pretty`/`json`/`toon`), `--fix`/dry-run-by-default, and the `poly` CLI plus its
> `polylint`/`polyfmt` aliases. Native backends are wired for JS/TS/JSON (oxc), Python (ruff),
> TOML (taplo), Markdown (rumdl), SQL (sqruff), CSS/SCSS/Less (malva), HTML/Vue/Svelte
> (markup_fmt), GraphQL, Nix (alejandra), and YAML, with cross-cutting spell-check (typos); every
> other language is covered by the tree-sitter generic tier. Still in progress: closing the
> conformance gap with per-language idiomatic formatters — see [Roadmap](#roadmap).

## Why

A typical repo today wires a dozen tools into `.pre-commit-config.yaml`, each with its own
runtime, its own config dialect, and its own install story. That is slow to set up, painful in
CI, and impossible to reproduce without the matching toolchains on every machine.

polylint collapses that into:

- **Two binaries** instead of a dozen tools and their language runtimes.
- **One config** (`polylint.toml`) instead of a config file per tool.
- **Zero system dependencies** — no Python, Node, Go, or JVM required, ever. Pure Rust,
  in-process; tree-sitter grammars for the generic tier are downloaded and cached on demand.
- **Opinionated, consistent defaults** — line length 120, LF endings, final newline, trailing
  whitespace trimmed, docstring code formatted — so there is nothing to bikeshed.

## Install

```sh
cargo install poly-cli   # the primary CLI: poly lint, poly fmt
```

Or install the alias binaries individually:

```sh
cargo install polylint   # alias for `poly lint`
cargo install polyfmt    # alias for `poly fmt`
```

> **Note:** the `0.0.1` releases on crates.io are name-reservation stubs. Functional
> releases ship as new backends land (see [Roadmap](#roadmap)).

## Usage

### Lint

```sh
poly lint [PATHS]...

  --fix                       Apply autofixes in place (default: dry run)
  --format <pretty|json|toon> Output format (default: pretty)
  --config <PATH>             Use an explicit config (default: nearest polylint.toml)
  --no-cache                  Bypass the result cache
  -j, --jobs <N>              Parallel jobs (default: all logical cores)
  --no-color                  Disable colored output
```

Or use the alias: `polylint [PATHS]...` with the same flags.

By default `poly lint` only reports. With `--fix` it applies the autofixes carried by
diagnostics (re-linting until stable) and rewrites the file, then reports whatever remains.

### Format

```sh
poly fmt [PATHS]...

  --fix                       Write formatted output in place (default: dry run)
  --check                     Explicit dry run (the default); conflicts with --fix
  --format <pretty|json|toon> Output format (default: pretty)
  --config <PATH>             Use an explicit config (default: nearest polylint.toml)
  --no-cache                  Bypass the result cache
  -j, --jobs <N>              Parallel jobs (default: all logical cores)
  --no-color                  Disable colored output
```

Or use the alias: `polyfmt [PATHS]...` with the same flags.

`poly fmt` is dry-run by default: it reports what would change, writes nothing, and exits
non-zero if any file would change — ideal for CI and pre-commit. Pass `--fix` to rewrite files
in place. `--check` is an explicit alias for the default dry run.

`PATHS` defaults to the current directory. Files are discovered through the `ignore` crate, so
`.gitignore` is respected.

### Exit codes

All binaries use exit codes designed to drive CI and git hooks:

| Code | `poly lint` / `polylint`          | `poly fmt` / `polyfmt`                      |
|------|-----------------------------------|---------------------------------------------|
| `0`  | No issues (or all autofixed)      | `--fix`: written; dry run: nothing to change |
| `1`  | Lint issues remain                | Dry run (default): at least one file would change |
| `2`  | Internal error (e.g. bad config)  | Internal error (e.g. bad config)            |

## Configuration

Configuration is a single canonical `polylint.toml`, discovered by walking up from the working
directory. It has three sections:

- `[defaults]` — the opinionated global knobs, applied wherever a tool exposes them.
- `[lint.<lang>.<tool>]` — per-language, per-tool lint options.
- `[fmt.<lang>.<tool>]` — per-language, per-tool format options.

Settings layer as **tool defaults → polylint's opinionated overrides → your `polylint.toml`**,
so you only ever write down what you want to change.

The opinionated defaults are: **line length 120**, **LF** line endings, a **final newline**,
**trailing whitespace trimmed**, and (for languages that support it) **docstring code always
formatted** at the same line length.

### Example

```toml
[defaults]
line_length = 120
line_ending = "lf"        # "lf" | "crlf"
final_newline = true
trim_trailing_whitespace = true

# Per-language, per-tool lint options
[lint.python.ruff]
# ruff-specific rule selection goes here

# Per-language, per-tool format options
[fmt.python.ruff]
docstring_code_format = true
docstring_code_line_length = 120
```

> Tables for tools that are not yet implemented are accepted and reserved; they take effect as
> the corresponding backends land.

## Architecture

polylint is a Cargo workspace built around a single `Engine` trait, with a deliberately small
two-tier coverage model:

1. **Native Rust crate backends** for languages that have a high-quality Rust tool — wrapped
   directly (or vendored when a crate doesn't externalize the API we need). These give
   first-class, language-aware lint + format fidelity. *(Planned: oxc, ruff, taplo, rumdl,
   sqruff, malva, markup_fmt, graphql, nixpkgs-fmt, typos.)*
2. **A tree-sitter generic formatter** built on `tree-sitter-language-pack` for *everything
   else* — the long tail of 300+ grammars. It parses the CST and re-emits the source with
   structural reindentation and whitespace normalization. This is best-effort rather than
   idiomatic per-language reflow, but it is pure Rust with zero system tools, which is the whole
   point. _(Planned.)_

Every backend implements the same `Engine` trait — `name`, `languages`, `capabilities`,
`version`, `lint`, `format` — and produces normalized `Diagnostic` and `FormatOutput` values, so
the runner, cache, and reporters are uniform across all languages. A static **registry** maps
each `Language` to its ordered list of engines (native first, generic tier as catch-all).

Cross-cutting machinery, all in place today:

- **blake3 content-hash cache** keyed over `(file bytes + engine name + engine version + resolved
  engine config)`, so a tool upgrade or config change invalidates stale results. `--no-cache`
  bypasses it.
- **rayon parallelism** across discovered files, using all logical cores by default (`-j` to cap).
- **`ignore`-based discovery** that honors `.gitignore`.

### Workspace layout

```text
polylint/
├── crates/
│   ├── polylint-core/   # engine library: trait, registry, config, cache, runner, reporting
│   │   └── src/engines/ # backends (whitespace today; native + tree-sitter to come)
│   ├── poly-cli/        # `poly lint` / `poly fmt` — shared library + CLI
│   ├── polylint/        # `polylint` binary (alias for `poly lint`)
│   ├── polyfmt/         # `polyfmt` binary (alias for `poly fmt`)
│   └── conformance/     # dev-only: differential test harness vs reference formatters
```

## Language & backend support

Coverage is delivered by backends. **Today only the reference whitespace backend is implemented**,
and the registry routes every language to it — so all "languages" below currently receive
whitespace-only normalization, not language-aware processing. The table tracks the intended
backend per language and its status.

| Language(s)                 | Intended backend         | Lint | Format | Status                |
|-----------------------------|--------------------------|------|--------|-----------------------|
| _(any text file)_           | whitespace (reference)   | ✅   | ✅     | **Done** (M1)         |
| JS / TS / JSX / TSX         | oxc                      | ✅   | ✅     | Planned (M2)          |
| JSON / JSONC                | oxc                      | —    | ✅     | Planned (M2)          |
| YAML                        | oxc / saphyr             | ✅   | ✅     | Planned (M2 / M4)     |
| Python                      | ruff                     | ✅   | ✅     | Planned (M3)          |
| Markdown                    | rumdl                    | ✅   | ✅     | Planned (M4)          |
| SQL                         | sqruff                   | ✅   | ✅     | Planned (M4)          |
| TOML                        | taplo                    | ✅   | ✅     | Planned               |
| CSS / SCSS / Less           | malva                    | —    | ✅     | Planned (M6)          |
| HTML / Vue / Svelte         | markup_fmt               | —    | ✅     | Planned (M6)          |
| GraphQL                     | graphql                  | —    | ✅     | Planned (M6)          |
| Nix                         | nixpkgs-fmt              | —    | ✅     | Planned (M6)          |
| _(all languages)_           | typos (spell-check)      | ✅   | —      | Planned (M6)          |
| Shell, Go, Java, Kotlin,    | tree-sitter generic tier | —    | ✅     | Planned (M5)          |
| Ruby, PHP, Elixir, C/C++,   |                          |      |        |                       |
| Rust, Dockerfile, proto, …  |                          |      |        |                       |

The generic tree-sitter tier (M5) is what guarantees the "no system dependencies" promise for the
long tail; native backends then progressively upgrade individual languages to higher fidelity.

## Roadmap

- **M0 — Reserve names.** `polylint` + `polyfmt` reserved on crates.io at `0.0.1`. ✅
- **M1 — Foundation + one backend end-to-end.** Workspace, `Engine` trait, registry, config +
  opinionated defaults, blake3 cache, `ignore` discovery, rayon runner, human + JSON reporting,
  both CLIs, and the reference whitespace backend. ✅
- **M2 — oxc backend.** JS/TS/JSX/TSX lint + format; JSON/YAML format.
- **M3 — ruff backend.** Python lint + format, with docstring code formatting on.
- **M4 — rumdl + sqruff + YAML.** Markdown, SQL, and YAML validity.
- **M5 — Tree-sitter generic tier.** The CST-driven reindent/whitespace formatter as the
  catch-all for every unmatched language — the milestone that delivers true zero system deps.
- **M6 — Fast-follow native backends.** malva (CSS/SCSS/Less), markup_fmt (HTML/Vue/Svelte),
  graphql, nixpkgs-fmt (Nix), typos (cross-language spell-check).
- **M7 — Polish + pre-commit.** `polylint --fix` / `polyfmt --check` hardening, cache-invalidation
  tests, a shipped `.pre-commit-hooks.yaml`, config-schema docs, CI, and a `cargo deny` license
  gate.
- **Ongoing.** Progressively port more of the tree-sitter tail to native Rust backends for
  higher-fidelity formatting.

### Pre-commit

Add polylint to your repo's `.pre-commit-config.yaml`:

```yaml
- repo: https://github.com/Goldziher/polylint
  rev: v0.1.0  # or whichever version
  hooks:
    - id: polylint
    - id: polyfmt
```

This replaces your entire per-language pre-commit hook stack (ruff, oxlint, shfmt, taplo,
rumdl, etc.) with just two hooks. See `.pre-commit-hooks.yaml` for hook configuration and
options.

## Contributing

Contributions are welcome. See [CLAUDE.md](CLAUDE.md) for architecture, conventions, and
instructions. The project is greenfield and the architecture (the `Engine` trait + static
registry) is intended to make adding a backend a self-contained unit of work. Each native
backend begins with an empirical check that the upstream crate externalizes the API we need; if
it doesn't, the source is vendored and recorded in `ATTRIBUTIONS.md`. No subprocesses, no
system dependencies — ever.

## License

Licensed under either of:

- Apache License, Version 2.0
- MIT license

at your option (**MIT OR Apache-2.0**).
