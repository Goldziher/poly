# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres
to [Semantic Versioning](https://semver.org/spec/v2.0.0.html). `polylint` and
`polyfmt` ship and version in lock-step.

## [Unreleased]

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
