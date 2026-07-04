# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres
to [Semantic Versioning](https://semver.org/spec/v2.0.0.html). `polylint` and
`polyfmt` ship and version in lock-step.

## [Unreleased]

## [0.5.1] - 2026-07-04

### Fixed

- **Trailing whitespace no longer leaks into `poly lint`.** The tree-sitter
  generic tier (and the format-only native-tool backends that fall back to it —
  `gofmt`, `rustfmt`, `swift-format`, …) previously reported a
  `trailing-whitespace` **lint** diagnostic that `poly lint --fix` could not act
  on: the diagnostic carried no autofix, and the fix lives on the format path.
  Worse, `lint` flagged it even in files that `fmt` deliberately leaves alone
  (e.g. a Swift file marked `// swift-format-ignore-file`), so the warning could
  never be cleared. Trailing whitespace is now purely a **`polyfmt`** concern:
  the generic tier and the format-only native backends declare `lint: false` and
  emit no lint diagnostics; run `poly fmt --fix` to strip trailing whitespace.

## [0.5.0] - 2026-07-04

### Added

- **Live per-hook progress for `poly hooks`** — when stderr is a terminal, each
  hook now prints a `▶ <id> …` line as it starts and a `✓/× <id> (<duration>)`
  line as it finishes, so a long-running hook (`cargo clippy`, `cargo test`, …) is
  visibly running instead of leaving the terminal blank until the whole stage
  completes — which read as a hung commit. Progress goes to stderr and is
  suppressed when stderr is not a terminal (piped / CI), so captured output is
  unchanged.
- **Autofixable count in the lint summary** — `poly lint` now reports how many of
  the findings can be resolved automatically (`N fixable with the `--fix`
  option.`), making the value of a follow-up `--fix` run obvious from a dry run.
  The line is omitted when nothing is fixable.

## [0.4.0] - 2026-07-04

### Added

- **Colored `poly hooks install` / `uninstall` output** — a green ✓ header with
  the hook count and the (relative) hooks directory, then one line per hook name,
  replacing the flat list of absolute paths.

### Changed

- **Installed git-hook shims resolve `poly` from `PATH`** rather than baking in an
  absolute path to the binary, so a hook always runs whatever `poly` is current
  (a recorded absolute path could pin a stale or moved build). When `poly` is not
  on `PATH` the shim now fails with a clear, actionable message and a non-zero exit
  instead of proceeding as though the hook had passed. Re-run `poly hooks install`
  to migrate existing shims.

### Fixed

- Native-toolchain formatter output is normalized to LF line endings; some
  first-party CLIs emit CRLF on Windows, which made output platform-dependent.

## [0.3.0] - 2026-07-04

### Added

- **Native-toolchain formatter backends** — opt-in backends that invoke a
  language's canonical first-party CLI when it is present on the host: Java,
  Kotlin, R, Swift, Dart, and Gleam. Off by default (enabled per-tool in config);
  when the tool is absent the language falls through to the tier-2 tree-sitter
  formatter, so the zero-dependency guarantee is intact for anyone who has not
  opted in.
- **C# tier-2 support** — a `Language::CSharp` variant so `.cs` files route to the
  tree-sitter generic formatter (deterministic, zero system dependency) instead of
  being skipped. Maps the `c#` / `csharp` catalog names and the `.cs` extension.
- **Elixir `do…end` reindent** in the tier-2 formatter. Elixir's blocks are
  keyword-delimited (`do…end`), so they matched neither the brace-counting path nor
  a language-pack indents query (tree-sitter-elixir ships none) and were left at
  column 0. A new built-in-indents-query dispatch slot plus a minimal Elixir query
  produces `mix format`'s 2-space nesting; idempotent, with heredocs/strings
  preserved.

### Changed

- **`poly fmt` honors `// swift-format-ignore-file`** — a Swift file carrying the
  directive is left byte-for-byte untouched (the same whole-file skip marker
  `swift-format` respects), mirroring the generated-lock-file skip. Protects files a
  project opted out of formatting and machine-generated swift-bridge glue.
- Bumped `fs-err`, `serde-saphyr`, and `sqruff` to their latest releases.

### Fixed

- **`poly hooks` now enforces the `commit-msg` stage.** Lowered hooks kept
  `Stage::default()` (pre-commit), so the runner dispatched the `poly commit`
  (Conventional Commits) builtin in file-input mode, matched no files, and silently
  skipped it — the git `commit-msg` hook never enforced anything. Every lowered hook
  is now stamped with the stage it was lowered for; latent for any non-pre-commit
  builtin, only `poly-commit` surfaced it.
- **Rust files named like `dockerfile.rs` are no longer misdetected as
  Dockerfiles.** Language detection now lets a known file extension (`.rs` → Rust)
  win over the Dockerfile filename match, so `engines/dockerfile.rs` and similar no
  longer produce spurious Dockerfile parse errors.

## [0.2.0] - 2026-07-03

### Added

- **Biome CSS + GraphQL linters** — two in-process tier-1 lint backends built on
  the official `biomejs/biome` analyzer crates, filling gaps polylint had no
  native linter for. Both are lint-only and coexist with the existing malva/
  graphql formatters. Configured via `[lint.css.biome]` / `[lint.graphql.biome]`
  with the shared `select`/`extend_select`/`ignore` surface; default rule groups
  are `correctness` + `suspicious`.
- **`poly migrate`** — new subcommand that absorbs a repo's `ruff` / `typos` /
  `taplo` / markdownlint config (including `pyproject.toml` `[tool.ruff]` /
  `[tool.typos]` / `[tool.codespell]`) into `poly.toml`, comment-preserving, then
  deletes or strips only the sources poly can fully honor. Dry-run report by
  default; `--write`, `--recurse`, `--verify`, `--strip-superseded`.
- **Native typos config** — `_typos.toml` / `.typos.toml` / `pyproject
  [tool.typos]` / `[tool.codespell]` are honored, including `extend-ignore-re`
  (region masking), `extend-ignore-words-re` / `-identifiers-re`, and full
  ancestor-chain merging.
- **Dockerfile rule selection** — the Dockerfile backend now honors
  `[lint.dockerfile]` `select` / `extend_select` / `ignore`.

### Changed

- Dry-run `poly fmt` (no `--fix`) now reports "N file(s) will change" instead of
  the past-tense "N changed", which implied files were rewritten.
- Bumped the pinned oxc (`c0c69dc`) and ruff (`1cb2012`) revisions and ran
  `cargo upgrade --incompatible` (clap_complete, rand, rmcp, rustc-hash).

### Removed

- **The R (air/jarl) tier-1 backend.** Migrating air+jarl onto official
  `biomejs/biome` was disproportionately costly (a large fork rebase across biome
  API drift plus a non-upstream patch), and air/jarl were the sole consumers of
  the `lionel-/biome` fork. Dropping them removes that fork from the dependency
  graph and unblocks the official biome CSS/GraphQL analyzers with no crate
  collision. R now falls through to the tier-2 tree-sitter formatter (best-effort
  format, no lint).

## [0.1.15] - 2026-07-02

### Added

- **Hierarchical (monorepo-aware) config resolution** (ADR 0018). Running `poly`
  from a monorepo root now discovers nested `poly.toml` files and cascades them
  the way ruff/eslint resolve config: a file is governed by the deep-merge of its
  ancestor config chain (workspace root as base, nearest config wins), so a
  sub-project's `poly.toml` declares only its diff and inherits `[defaults]`, the
  `[lint.*]`/`[fmt.*]` rule tables, and `[per-file-ignores]` from above.
  - New `[workspace] root = true` marker bounds the upward cascade; a `.git`
    directory is an implicit boundary, so single repos need no annotation.
  - `[discovery] exclude` globs are unioned tree-wide, each rooted at its own
    config directory (a nested config prunes only its subtree); `[per-file-ignores]`
    globs resolve relative to their owning config's directory.
  - `--config <path>` pins a single config and bypasses nested resolution.
  - Fully back-compatible: a repo with one root `poly.toml` and no nested configs
    resolves every file to the root config, identical to before.

## [0.1.12] - 2026-07-02

### Fixed

- **ruff cache-key** now folds the E501/`line_length` engine change (0.1.11).
  The `line_length`-honoring fix altered lint output for the same input without
  bumping the ruff engine `version()` suffix, so warm `.polylint` caches kept
  serving stale 88-column E501 diagnostics. Bumped the suffix (`+e501`) to
  invalidate them. (CI is unaffected — fresh runners have no cache.)

## [0.1.11] - 2026-07-02

### Fixed

- **ruff E501 now honors `line_length`.** The line-too-long rule read ruff's
  `pycodestyle.max_line_length`, which poly never set — so it stayed pinned at
  ruff's hardcoded 88 regardless of the configured `line_length` (while the
  formatter correctly used 120). poly now mirrors the resolved `line_length`
  onto `pycodestyle.max_line_length` in both the default and per-config settings,
  so `select = ["ALL"]` projects with a 120 limit no longer see false-positive
  E501 on 89–120 char lines.

## [0.1.10] - 2026-07-02

### Fixed

- **actionlint**: restrict linting to GitHub Actions workflow files
  (`.github/workflows/**/*.yml|yaml`). Previously `poly lint .` ran `actionlint`
  on every YAML file (including `Taskfile.yml`, `docker-compose.yaml`, etc.),
  emitting spurious "jobs section is missing" errors. The tool now silently skips
  non-workflow YAML. A new `path_globs` field in the catalog model provides a
  general mechanism for future path-scoped tools.

### Added

- **ruff / isort**: `known_first_party` and `known_third_party` options for the
  ruff engine, settable in `poly.toml` under `[lint.python.ruff]`. Resolves
  false `I001` (import-block un-sorted) errors when a first-party package lives
  in a `src/`-layout that the package-root walk cannot reach from a sibling
  `tests/` directory.

  ```toml
  [lint.python.ruff]
  known_first_party = ["kreuzberg_cloud"]
  ```

## [0.1.9] - 2026-07-02

### Fixed

- **rustfmt**: `poly fmt` now honours the project's `rustfmt.toml` /
  `.rustfmt.toml` when formatting Rust source. Previously poly always injected
  `--config max_width=120`, which silently overrode every option in the
  project's rustfmt config (not just `max_width`). Now poly walks up from each
  source file to find the nearest `rustfmt.toml` and passes its directory via
  `--config-path`, letting rustfmt load the full project configuration. When no
  config file is found the existing 120-column default is preserved.

## [0.1.8] - 2026-07-01

### Fixed

- **cli**: `poly lint` exits non-zero only when a diagnostic is error-severity.
  Warning/info/hint findings are still reported but no longer fail the run, git
  hooks, or CI — matching the ruff/eslint/clippy convention. Previously any finding
  (including warnings) exited non-zero.

### Changed

- **deps**: upgrade dependencies to their latest versions (`cargo upgrade --incompatible`):
  quick-xml 0.40 → 0.41, plus clap_complete and indicatif.

## [0.1.7] - 2026-07-01

### Added

- **Uniform rule selection across `ruff`, `sqruff`, and `rumdl`** — all three now
  accept the canonical `select` / `extend_select` / `ignore` vocabulary through the
  shared parser, with each tool's native keys (`rules`/`exclude_rules`,
  `enable`/`disable`) kept as back-compat aliases and unioned. Unknown or blank rule
  codes are surfaced with a warning and skipped instead of dropped silently.
- **Uniform per-rule severity remap** — a configured `[lint.<lang>.<tool>.rules.<code>]
  level` is now honored for every engine as a post-lint remap on the normalized
  diagnostic code, including engines with no native severity configuration.
- ADR 0016 (uniform rule-selection model) and ADR 0017 (path exclusions and
  per-file rule ignores) documenting the configuration design.

### Fixed

- **Tier-2 generic formatter** no longer rewrites the interior of multi-line
  strings, heredocs, raw strings, or block comments on the query-driven reindent
  path (the brace-counting path was already guarded); their significant leading
  whitespace is preserved byte-for-byte.
- **`rubyfmt` cache key** now folds the pinned git rev instead of a stale version
  string, so a rev bump invalidates cached Ruby output.

### Changed

- **Homebrew distribution now ships bottles.** The tap formula builds `poly` from
  source and the release dispatches the tap's bottle workflow, so `brew install`
  pours a prebuilt bottle on supported platforms (macOS ARM64, Linux x86_64/ARM64)
  and builds from source elsewhere.

### Internal

- Extracted a shared `deserialize_options` helper for the format-only backends
  (malva, markup_fmt).
- Added a `Cargo.lock` drift-guard test asserting every backend's `version()`
  embeds the resolved crate version or pinned git rev — enforcing cache-key
  discipline across all 17 backends.

## [0.1.6] - 2026-06-30

### Added

- **Per-tool rule configuration** — a uniform `select` / `extend_select` / `ignore`
  surface plus per-rule `[rules.<id>]` overrides (`level` + tool-specific params)
  for `mago`, `ruff`, `oxc`, `sqruff`, `rumdl`, and R/`jarl`. `select` by category
  replaces the default set; unknown rule/category names error loudly.
- **Formatter options** via `[fmt.<lang>.<tool>]` for `yaml`, CSS/SCSS/Less
  (`malva`), HTML/Vue/… (`markup_fmt`), GraphQL, and TOML (`taplo`).
- **ruff per-plugin parameters**: `pydocstyle_convention`, `mccabe_max_complexity`,
  `pylint_max_args`, `pylint_max_branches`, `pylint_max_returns`.
- **Path exclusions** across config, CLI, and MCP: `[discovery] exclude`, a
  repeatable `--exclude <glob>` flag, and an MCP `exclude` parameter.
- **`[per-file-ignores]`** — gitignore-style glob → rule-code suppression, applied
  as a cross-engine post-lint filter.
- **`[tools.*]` `env` and `root`** — environment variables and a working directory
  for catalog tools (e.g. running `golangci-lint` per Go module).
- **`[hooks.builtin.cargo] clippy_args`** to override the clippy invocation.
- **oxc** per-rule `Deny` severity and JS/JSON formatter options.
- Per-engine `indent_width` override (honored uniformly by every formatter).
- A `Taskfile.yaml` with the standard dev tasks.

### Fixed

- **ruff INP001 false positives** and **isort first-party misclassification** —
  the package root is now resolved from the file's directory, so per-file linting
  matches ruff's whole-tree behavior. (isort `I001`/`I002` are in the default set,
  so this affected every run.)
- **Single-file invocations** now apply `[per-file-ignores]` and report the
  correct path (a file passed as its own root collapsed to an empty match path).
- **Lint cache correctness** — the cache key now folds the file path (byte-identical
  files such as empty `__init__.py` no longer collide and serve each other's
  path-dependent diagnostics) and the effective `[defaults]` globals.
- **HCL** inline trailing comments are no longer lost on format (files with
  comments route to the structural tier instead of the comment-stripping path).
- **Dockerfile** parse failures now surface as `Error` diagnostics instead of
  being silently swallowed.
- **sqruff** parse/lex errors are reported as `Error` (not `Warning`).
- **R** `--fix` applies only fixes whose status is `Safe`.
- **rustfmt** (native-toolchain backend) honors the 120-column line width.
- **`php_version`** rejects a non-numeric component (e.g. `"8.x"`) instead of
  silently defaulting it to `0`.

### Performance

- **mago** caches its rule registry per run instead of rebuilding it for every file.

### Changed

- `oxc` and `sqruff` no longer advertise a `fix` capability they did not implement.

### Documentation

- Corrected ADR drift (configuration, backend selections, distribution, catalog,
  caching) and README inaccuracies (version-pin examples, MCP tool names and
  parameters, version badges).

[0.1.6]: https://github.com/Goldziher/polylint/releases/tag/v0.1.6
