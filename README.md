# polylint

[![CI](https://github.com/Goldziher/polylint/actions/workflows/ci.yml/badge.svg)](https://github.com/Goldziher/polylint/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)

**One linter, one formatter, one commit-and-hooks toolchain for every language — pure Rust,
in-process, zero system dependencies.**

`poly` is a single CLI that replaces your entire per-language tooling stack — `ruff` + `oxlint` +
`oxfmt` + `taplo` + `rumdl` + `sqruff` + `shfmt` + `clang-format` + `pre-commit` + commit-message
linters + … and all of their runtimes (Python, Node, Go, a JVM, …) — with **one binary and one
`poly.toml`**:

```text
poly lint     # lint every language in the repo
poly fmt      # format every language in the repo
poly commit   # lint & clean commit messages (Conventional Commits)
poly hooks    # run git hooks declared in poly.toml
```

Everything runs **in-process, in pure Rust**. There are no subprocesses and nothing to install on
the host: where a high-quality Rust crate exists for a language it is compiled in directly, and
everything else is covered by a generic tree-sitter formatter whose grammars are fetched on demand.

## Why

A typical repo wires a dozen tools into `.pre-commit-config.yaml`, each with its own runtime, its
own config dialect, and its own install story. That is slow to set up, painful in CI, and
impossible to reproduce without the matching toolchains on every machine.

polylint collapses that into:

- **One binary** (`poly`, plus `polylint`/`polyfmt` aliases) instead of a dozen tools and their
  language runtimes.
- **One config** (`poly.toml`) for linting, formatting, commit-message rules, and git hooks.
- **Zero system dependencies** — no Python, Node, Go, or JVM required, ever. Pure Rust, in-process;
  tree-sitter grammars for the generic tier are downloaded and cached on demand.
- **Opinionated, consistent defaults** — line length 120, LF endings, final newline, trailing
  whitespace trimmed, docstring code formatted — so there is nothing to bikeshed.

## Install

polylint ships **prebuilt, platform-specific binaries** attached to each GitHub release (it is
distributed like `ruff`/`biome`/`oxlint`, not published to crates.io).

**Prebuilt binary (recommended):**

```sh
cargo binstall poly-cli            # fetches the release artifact for your platform
```

Or download the archive for your target from the
[releases page](https://github.com/Goldziher/polylint/releases) and put `poly` (and the
`polylint`/`polyfmt` aliases) on your `PATH`. Each release ships a `sha256sums.txt`.

**From source:**

```sh
git clone https://github.com/Goldziher/polylint && cd polylint
cargo build --release           # binaries land in target/release/{poly,polylint,polyfmt}
```

## Quickstart

```sh
poly fmt            # show what would change (dry run, default)
poly fmt --fix      # format the whole repo in place
poly lint           # report lint diagnostics
poly lint --fix     # apply autofixes, then report what remains
```

`PATHS` default to the current directory; files are discovered through the `ignore` crate, so
`.gitignore` is respected.

## Usage

### `poly lint` / `poly fmt`

```sh
poly lint [PATHS]...
poly fmt  [PATHS]...

  --fix                        Apply changes in place — autofixes for lint, formatting for fmt
                               (default: dry run that writes nothing)
  --check                      (fmt only) explicit dry run; conflicts with --fix
  --format <pretty|json|toon>  Output format (default: pretty)
  --config <PATH>              Use an explicit config (default: nearest poly.toml)
  --no-cache                   Bypass the result cache
  -j, --jobs <N>               Parallel jobs (default: all logical cores)
  --no-color                   Disable colored output
```

The `polylint` and `polyfmt` binaries are thin aliases for `poly lint` and `poly fmt` with the same
flags. `poly fmt` is **dry-run by default**: it reports what would change, writes nothing, and exits
non-zero if any file would change — ideal for CI and pre-commit. Pass `--fix` to rewrite in place.

Three output formats are available everywhere: **`pretty`** (colored, human-oriented), **`json`**,
and **`toon`** (Token-Oriented Object Notation, compact for LLM/agent consumption).

### `poly commit`

Lints and optionally cleans a commit message against Conventional Commits, and strips AI-attribution
trailers. Driven by the `[commit]` table in `poly.toml` (or a native `.gitfluff.toml`). Powered by
the bundled `gitfluff` engine, which also ships as a standalone binary.

### `poly hooks`

Runs the git hooks you declare in the `[hooks]` table of `poly.toml`. poly's own tools
(`polylint`/`polyfmt`/`poly commit`) run as first-class hooks, and foreign pre-commit repos are
cloned and run through the bundled `prek` engine — so `poly hooks` is a drop-in for `pre-commit`
that needs no Python.

```sh
poly hooks run --all-files     # arguments after `hooks` forward to the engine
poly hooks install
```

### Exit codes

| Code | `poly lint` / `polylint`         | `poly fmt` / `polyfmt`                            |
|------|----------------------------------|---------------------------------------------------|
| `0`  | No issues (or all autofixed)     | `--fix`: written; dry run: nothing to change      |
| `1`  | Lint issues remain               | Dry run (default): at least one file would change |
| `2`  | Internal error (e.g. bad config) | Internal error (e.g. bad config)                  |

## Configuration

Configuration is a single canonical **`poly.toml`**, discovered by walking up from the working
directory (`polylint.toml` is read as a back-compat fallback; `poly.toml` wins within a directory).
Settings layer as **tool default → polylint's opinionated override → your `poly.toml`**, so you only
write down what you want to change.

```toml
[defaults]
line_length = 120
line_ending = "lf"            # "lf" | "crlf"
final_newline = true
trim_trailing_whitespace = true

# Per-language, per-tool lint and format options
[lint.python.ruff]
# ruff rule selection …

[fmt.python.ruff]
docstring_code_format = true
docstring_code_line_length = 120

# Commit-message rules (poly commit)
[commit]
preset = "conventional"

# Git hooks (poly hooks) — replaces .pre-commit-config.yaml
[hooks]
stages = ["pre-commit"]
[hooks.builtin]
polylint = true
polyfmt  = true
commit   = true               # runs at the commit-msg stage
```

The opinionated defaults are: **line length 120**, **LF** endings, a **final newline**, **trailing
whitespace trimmed**, and (where supported) **docstring code formatted** at the same line length.

## Language & backend support

polylint uses a **two-tier coverage model**: a native Rust crate backend where a high-quality one
exists, and a tree-sitter generic tier for everything else. A static registry maps each language to
its engines; `typos` spell-checks every file in addition to its language-specific engine.

| Language(s)                       | Backend                          | Lint | Format |
|-----------------------------------|----------------------------------|------|--------|
| JS / TS / JSX / TSX               | oxc (oxlint + oxc_formatter)     | ✅   | ✅     |
| JSON / JSONC                      | oxc (oxc_formatter)              | —    | ✅     |
| Python                            | ruff                             | ✅   | ✅     |
| TOML                              | taplo                            | ✅   | ✅     |
| Markdown                          | rumdl                            | ✅   | ✅     |
| SQL                               | sqruff                           | ✅   | ✅     |
| YAML                              | pretty_yaml (saphyr)             | ✅   | ✅     |
| CSS / SCSS / Less                 | malva                            | —    | ✅     |
| HTML / Vue / Svelte               | markup_fmt                       | —    | ✅     |
| GraphQL                           | pretty_graphql                   | —    | ✅     |
| Nix                               | alejandra                        | —    | ✅     |
| Ruby                              | rubyfmt                          | —    | ✅     |
| PHP                               | mago                             | ✅   | ✅     |
| _(every file)_                    | typos (spell-check)              | ✅   | —      |
| Shell, Go, Java, Kotlin, Rust,    | tree-sitter generic tier         | —    | ✅     |
| C/C++, Swift, Dart, C#, Zig,      | (300+ grammars, structural       |      |        |
| Dockerfile, protobuf, R, …        | reindent + whitespace)           |      |        |

The generic tree-sitter tier is what guarantees the "no system dependencies" promise for the long
tail; native backends progressively upgrade individual languages to higher fidelity.

## Architecture

polylint is a Cargo workspace (Rust 2024) built around a single `Engine` trait:

```rust
pub trait Engine: Send + Sync {
    fn name(&self) -> &'static str;
    fn languages(&self) -> &[Language];
    fn capabilities(&self) -> Capabilities;   // lint / format / fix
    fn version(&self) -> &str;                 // folded into the cache key
    fn lint(&self, src: &SourceFile, cfg: &EngineConfig) -> Result<Vec<Diagnostic>>;
    fn format(&self, src: &SourceFile, cfg: &EngineConfig) -> Result<FormatOutput>;
}
```

Every backend — native or generic — produces normalized `Diagnostic` and `FormatOutput` values, so
the runner, cache, and reporters are uniform across all languages.

Cross-cutting machinery:

- **rayon parallelism** across discovered files, saturating all logical cores by default (`-j` caps).
- **blake3 content-hash cache** keyed over `(file bytes + engine name + engine version + resolved
  engine config)`, so a tool upgrade or config change invalidates stale results. `--no-cache`
  bypasses it.
- **`ignore`-based discovery** that honors `.gitignore`.
- **Shared `Arc<str>` file contents** so multiple engines lint/format one file without re-cloning it.

### Workspace layout

```text
polylint/
├── crates/
│   ├── polylint-core/   # engine library: Engine trait, registry, cache, runner, reporting, engines/
│   ├── poly-config/     # the unified poly.toml schema, shared by every surface
│   ├── poly-cli/        # the `poly` CLI (lint / fmt / commit / hooks) + shared run logic
│   ├── polylint/        # `polylint` binary (alias for `poly lint`)
│   ├── polyfmt/         # `polyfmt` binary (alias for `poly fmt`)
│   ├── gitfluff/        # Conventional-Commit linter/cleaner behind `poly commit` (also standalone)
│   ├── polyhooks/       # vendored prek — the git-hook engine behind `poly hooks`
│   └── conformance/     # dev-only differential test harness vs reference formatters (Docker)
```

## Pre-commit

Two ways to wire polylint into git hooks:

**The poly umbrella (recommended):** declare hooks in `poly.toml`'s `[hooks]` table and run
`poly hooks install` / `poly hooks run` — a drop-in `pre-commit` replacement with no Python.

**Classic pre-commit:** if your repo already uses [pre-commit](https://pre-commit.com), add the two
shipped hooks to `.pre-commit-config.yaml` to replace your whole per-language hook stack:

```yaml
- repo: https://github.com/Goldziher/polylint
  rev: v0.1.0
  hooks:
    - id: polylint
    - id: polyfmt
```

## Contributing

Contributions are welcome. See [CLAUDE.md](CLAUDE.md) for architecture, conventions, and the
backend-authoring workflow. The `Engine` trait + static registry are designed to make adding a
backend a self-contained unit of work: each native backend begins with an empirical check that the
upstream crate externalizes the API we need, ships a known-bad and a known-unformatted fixture, and
is wired into the registry. No subprocesses, no system dependencies — ever (the `poly hooks` engine
is the sole, deliberate exception, since running foreign hooks inherently shells out).

## License

Licensed under either of **MIT** or **Apache License, Version 2.0**, at your option
(**MIT OR Apache-2.0**).
