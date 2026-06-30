<!-- markdownlint-disable MD033 MD041 -->
<div align="center">

<img src="docs/media/polylint-banner.svg" alt="polylint — universal linter & formatter" width="820">

[![CI](https://img.shields.io/github/actions/workflow/status/Goldziher/polylint/ci.yaml?style=flat-square)](https://github.com/Goldziher/polylint/actions/workflows/ci.yaml)
[![npm](https://img.shields.io/npm/v/@nhirschfeld/polylint?style=flat-square)](https://www.npmjs.com/package/@nhirschfeld/polylint)
[![PyPI](https://img.shields.io/pypi/v/polylint?style=flat-square)](https://pypi.org/project/polylint/)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue?style=flat-square)](#license)

</div>

**Universal zero-dependency linter & formatter. Pure Rust, in-process, one config.**

`poly` is a single CLI that replaces your entire per-language tooling stack — `ruff` + `oxlint` +
`prettier` + `taplo` + `rumdl` + `sqruff` + `shfmt` + `rustfmt` + `gofmt` + `pre-commit` +
commit-message linters and all their runtimes (Python, Node, Go, a JVM, …) — with **one binary and
one `poly.toml`**:

```sh
poly lint       # lint every language in your repo
poly fmt        # format every language in your repo
poly fmt --fix  # apply formatting in place
poly commit     # lint commit messages (Conventional Commits)
poly hooks      # run git hooks declared in poly.toml
```

Everything runs **in-process, in pure Rust**. No subprocesses, no system dependencies. Where a
high-quality Rust library exists for a language, it is compiled in directly. Everything else is
covered by a generic tree-sitter formatter (300+ grammars, structural reindent) — so the
zero-dependency promise holds for every language on day one.

## Why

A typical repo chains a dozen tools into `.pre-commit-config.yaml`, each with its own runtime, config
dialect, and install story. That's slow to set up, painful in CI, impossible to reproduce without
matching toolchains on every machine.

poly collapses that stack:

- **One binary** (`poly`, plus `polylint` and `polyfmt` aliases) instead of a dozen tools and their runtimes.
- **One config** (`poly.toml`) for linting, formatting, commit rules, and git hooks.
- **Zero system dependencies** — pure Rust, in-process; tree-sitter grammars are fetched on demand.
- **Opinionated defaults** — line length 120, LF endings, final newline, trailing whitespace
  trimmed, docstring code formatted — nothing to bikeshed.
- **Content-hash caching** — unchanged files skip re-processing (blake3 cache key folds in file
  bytes + engine name + version + config).
- **Full parallelism** — rayon per-file parallelism saturates all logical cores by default.

## Install

`poly` ships as **prebuilt, platform-specific binaries** for Linux, macOS, and Windows (distributed
like `ruff` / `biome`, not published to crates.io).

**Installer script (recommended):**

```sh
curl -fsSL https://raw.githubusercontent.com/Goldziher/polylint/main/install.sh | sh
```

On Windows (PowerShell):

```powershell
irm https://raw.githubusercontent.com/Goldziher/polylint/main/install.ps1 | iex
```

Detects your platform and downloads the matching prebuilt binary, verifies it against
`sha256sums.txt`, and installs `poly`, `polylint`, and `polyfmt` into `~/.local/bin` (override with
`POLY_INSTALL_DIR`). **Re-run the same command any time to update to the latest release**; pin a
version with `POLY_VERSION=v0.1.0`.

**Homebrew:**

```sh
brew install Goldziher/tap/polylint
```

**npm:**

```sh
npm install -g @nhirschfeld/polylint
```

**PyPI:**

```sh
pip install polylint
```

The npm and PyPI packages are thin wrappers that download the matching prebuilt binary (verified
against `sha256sums.txt`) on first install; both install `poly`, `polylint`, and `polyfmt`.

**cargo-binstall:**

Because we don't publish to crates.io, point binstall at the repo directly:

```sh
cargo binstall --git https://github.com/Goldziher/polylint poly-cli
```

**Manual download:**

Grab the archive for your platform from the [releases page](https://github.com/Goldziher/polylint/releases),
extract, and add to your `PATH`. Each release includes `sha256sums.txt` for verification.

**From source:**

```sh
git clone https://github.com/Goldziher/polylint
cd polylint
cargo build --release  # binaries in target/release/{poly,polylint,polyfmt}
```

**GitHub Actions:**

```yaml
- uses: Goldziher/polylint@v0   # moving major tag; or pin a release: @v0.1.0
  with:
    version: latest  # or a specific release, e.g. v0.1.0
```

The action caches binaries by default and adds `poly` to `PATH`. Pass `cache: false` to skip caching.
`@v0` tracks the latest `0.x` release; pin `@v0.1.0` for a fixed version.

## Quickstart

```sh
# Dry run: show what would change (default)
poly fmt

# Apply formatting in place
poly fmt --fix

# Lint all files
poly lint

# Lint and apply autofixes
poly lint --fix

# Format in JSON
poly fmt --format json
```

Paths default to the current directory; `.gitignore` is respected.

## Usage

### `poly lint` / `poly fmt`

```text
poly lint [PATHS]...
poly fmt [PATHS]...

  --fix                        Apply changes in place (autofixes for lint, formatting for fmt).
                               Default: dry run.
  --check                      (fmt only) Explicit dry run. Conflicts with --fix.
  --format <pretty|json|toon>  Output format (default: pretty).
  --config <PATH>              Use an explicit config file (default: nearest poly.toml).
  --no-cache                   Bypass the result cache.
  -j, --jobs <N>               Parallel jobs (default: all logical cores).
  --no-color                   Disable colored output.
  --verbose                    (pretty format) Show extra detail: description, rule URL, metadata.
  --debug                      Emit cache hit/miss and timing; raise log verbosity to debug.
```

The `polylint` and `polyfmt` binaries are aliases for `poly lint` and `poly fmt` with the same flags.

**Exit codes:**

| Code | `poly lint`/`polylint` | `poly fmt`/`polyfmt` |
|------|---|---|
| 0 | No issues (or all autofixed) | `--fix`: changes applied; dry run: no changes needed |
| 1 | Lint issues remain | Dry run (default): at least one file would change |
| 2 | Internal error (config, I/O) | Internal error (config, I/O) |

### `poly fmt` dry-run by default

`poly fmt` **reports what would change, writes nothing, exits non-zero if any file would change**.
This is ideal for CI. Pass `--fix` to rewrite in place. The `--check` flag is an explicit alias for
the default dry-run behavior.

### Output formats

Three formats are available everywhere:

- **`pretty`** (default) — colored, human-oriented output with inline code snippets.
- **`json`** — fully structured JSON with all metadata.
- **`toon`** — Token-Oriented Object Notation, compact for LLM/agent consumption.

`--verbose` (pretty only) adds description, rule URL, and metadata. `--debug` emits per-engine cache
hit/miss and timing information.

### `poly commit`

Lints and optionally cleans a commit message against Conventional Commits. Reads from the `[commit]`
table in `poly.toml` (or a `.gitfluff.toml` file for back-compat). Strips AI-attribution trailers.

```sh
poly commit [MSG]
```

Driven by the bundled `gitfluff` engine (also available as a standalone binary).

### `poly hooks`

Runs git hooks declared in the `[hooks]` table of `poly.toml`. `poly`'s own tools (`polylint`,
`polyfmt`, `poly commit`) are first-class hooks. Foreign pre-commit repos are cloned and run through
the bundled `prek` engine — so `poly hooks` is a drop-in for `pre-commit` with no Python dependency.

```sh
poly hooks install          # install git-hook shims
poly hooks run pre-commit   # run the pre-commit stage
poly hooks run --all-files  # run all stages on all files
```

### `poly cache`

Inspect and maintain the blake3 result cache.

```sh
poly cache stats   # show cache statistics
poly cache size    # show cache size on disk
poly cache clean   # remove all cached results
```

### `poly mcp`

Run an MCP (Model Context Protocol) server over stdio. Mirrors the CLI surface via rmcp 2.0 — six
tools: `lint` / `format_check` / `cache_stats` (read-only) + `lint_fix` / `format_write` /
`cache_clean`.

```sh
poly mcp --config /path/to/poly.toml
```

The server reads `poly.toml` per request; `--config` provides a fallback for requests that don't specify their own.

## Configuration

Configuration is a single canonical **`poly.toml`**, discovered by walking up from the working
directory. (Back-compat: `polylint.toml` is read as a fallback; `poly.toml` wins in the same
directory.)

Settings layer as **tool default → poly's opinionated override → your `poly.toml`**, so you only
write what you want to change.

### Minimal example

```toml
[defaults]
line_length = 120
line_ending = "lf"
final_newline = true
trim_trailing_whitespace = true

[lint.python.ruff]
# Per-tool lint configuration (tool-specific TOML keys)

[fmt.python.ruff]
docstring_code_format = true
docstring_code_line_length = 120

[commit]
preset = "conventional"  # lint commit messages

[hooks]
stages = ["pre-commit"]

[hooks.builtin]
polylint = true
polyfmt = true
commit = { stages = ["commit-msg"] }
```

### Opinionated defaults

These apply everywhere a tool supports the setting:

- **Line length:** 120
- **Line endings:** LF (`\n`)
- **Final newline:** required
- **Trailing whitespace:** trimmed
- **Docstring code formatting:** enabled (where supported)

### Hooks

Declare hooks in the `[hooks]` table. `poly hooks install` writes git-hook shims.

```toml
[hooks]
stages = ["pre-commit", "commit-msg"]

[hooks.builtin]
# Built-in poly tools run first
polylint = true           # linting
polyfmt = true            # formatting
commit = { stages = ["commit-msg"] }  # commit messages

[hooks.pre-commit.scripts.my-script]
script = "scripts/my-hook.sh"
runner = "bash"
files = "**/*.rs"
```

### Catalog tools (optional)

Opt into 348 additional formatters/linters via the `[tools]` table (mdsf catalog):

```toml
[tools.prettier]
enabled = true
languages = ["javascript", "typescript"]

[tools.black]
enabled = true
languages = ["python"]
```

Catalog tools are capability-probed; a missing binary degrades gracefully to the native tier.

### Per-language, per-tool config

Language-specific, tool-specific options nest under `[lint.<lang>.<tool>]` or `[fmt.<lang>.<tool>]`:

```toml
[fmt.python.ruff]
indent_size = 4

[fmt.javascript.oxc]
max_line_length = 100

[lint.python.ruff]
line-length = 120
select = ["E", "W", "F"]  # flake8 rule codes
```

See each tool's documentation for available options.

## Language & backend support

Poly uses a **three-tier coverage model**:

1. **Native Rust backends (tier-1)** — highest-fidelity in-process libraries for specific languages.
2. **Tree-sitter generic tier (tier-2)** — structural reindent + whitespace normalization for 300+ grammars.
3. **Opt-in catalog tools** — 348 additional formatters/linters from mdsf, probed on `PATH`.

Additionally, opt-in **native-toolchain backends** wrap canonical first-party formatters (`gofmt`,
`rustfmt`, `zig fmt`) when present on the host. These are default-off; enable via
`[fmt.<lang>.<tool>] enabled = true`.

### Backend matrix

| Language(s) | Backend | Lint | Format |
|---|---|---|---|
| JavaScript / TypeScript / JSX / TSX | oxc (oxlint + oxc_formatter) | ✅ | ✅ |
| JSON / JSONC | oxc (oxc_formatter) | — | ✅ |
| Python | ruff | ✅ | ✅ |
| TOML | taplo | ✅ | ✅ |
| Markdown | rumdl | ✅ | ✅ |
| SQL | sqruff | ✅ | ✅ |
| YAML | yaml (saphyr) | ✅ | ✅ |
| CSS / SCSS / Less | malva | — | ✅ |
| HTML / Vue / Svelte / Astro | markup_fmt | — | ✅ |
| GraphQL | pretty_graphql | — | ✅ |
| HCL / Terraform | hcl (hcl-rs) | — | ✅ |
| Dockerfile | dockerfile (tree-sitter) | — | ✅ |
| Nix | alejandra | — | ✅ |
| Ruby | rubyfmt | — | ✅ |
| PHP | mago | ✅ | ✅ |
| R | air | — | ✅ |
| Go | gofmt (native-toolchain, default-on) | — | ✅ |
| Rust | rustfmt (native-toolchain, default-on) | — | ✅ |
| Zig | zig fmt (native-toolchain, opt-in) | — | ✅ |
| Shell | shfmt + shellcheck (native-toolchain, default-on) | ✅ | ✅ |
| _(all files)_ | typos (spell-check) | ✅ | — |
| Shell, Java, Kotlin, C/C++, Swift, Dart, C#, Protobuf, Haskell, … | tree-sitter (300+ grammars) | — | ✅ |

### Native-toolchain backends

Go's `gofmt`, Rust's `rustfmt`, and Shell's `shfmt`/`shellcheck` are **default-on** when detected on
`PATH`. When absent, these languages fall through to the tree-sitter generic tier (with an
info-level notice). Zig's `zig fmt` is **opt-in** (off by default) to keep the zero-dependency
promise intact for users who haven't asked for it.

Enable or disable per-tool in `poly.toml`:

```toml
[fmt.rust.rustfmt]
enabled = true

[fmt.zig.zig_fmt]
enabled = false  # use tree-sitter instead

[fmt.shell.shfmt]
enabled = true
```

## Architecture

`poly` is a Cargo workspace (Rust 2024) built around a single `Engine` trait. Every backend — native
or generic — produces normalized `Diagnostic` and `FormatOutput` values, so the runner, cache, and
reporters are uniform across all languages.

Cross-cutting machinery:

- **rayon `par_iter`** over discovered files, saturating available cores by default.
- **blake3 content-hash cache** keyed over `(file bytes + engine name + engine version + resolved
  config)`. A tool upgrade or config change invalidates stale results.
- **`ignore` crate discovery** respects `.gitignore`.
- **Shared `Arc<str>` file contents** so multiple engines process one file without re-cloning.

### Workspace layout

```text
crates/
├── polylint-core/        # Engine trait, registry, cache, runner, engines
├── poly-config/          # poly.toml schema, shared by all surfaces
├── poly-cli/             # poly CLI (lint / fmt / commit / hooks)
├── polylint/             # polylint binary (alias for `poly lint`)
├── polyfmt/              # polyfmt binary (alias for `poly fmt`)
├── gitfluff/             # Conventional-Commit linter (also standalone binary)
├── poly-hooks/           # Git-hook engine (vendored pre-commit fork)
├── poly-mcp/             # MCP server implementation
├── poly-cache/           # blake3 caching
├── poly-catalog/         # 348-tool mdsf registry
└── conformance/          # Differential testing harness (Docker)
```

## Git hooks

`poly hooks` is a self-contained git-hook runner — a drop-in for `pre-commit` with no Python
dependency. Declare hooks in `poly.toml`'s `[hooks]` table and run `poly hooks install`.

```toml
[hooks]
stages = ["pre-commit"]

[hooks.builtin]
polylint = true
polyfmt = true
commit = { stages = ["commit-msg"] }
```

Then:

```sh
poly hooks install
git commit  # hooks run automatically
```

`poly`'s own tools run as first-class hooks; foreign pre-commit repos referenced from `poly.toml` are
cloned and run through the bundled `prek` engine, so a single `poly hooks` run subsumes a much larger
per-language hook matrix.

## Contributing

Contributions are welcome. The codebase is organized around the `Engine` trait
(`crates/polylint-core/src/engine.rs`): each native backend is a self-contained implementation. See
[CLAUDE.md](CLAUDE.md) for architecture, conventions, and the workflow for adding a new backend.

The key rule: **no subprocesses, no system dependencies, ever** — except opt-in native toolchain
backends (which degrade gracefully when the tool is absent) and `poly hooks` (which must run foreign
hooks).

## License

Licensed under either of **MIT** or **Apache License, Version 2.0**, at your option
(**MIT OR Apache-2.0**).
