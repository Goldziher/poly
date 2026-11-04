---
type: Concept
title: Engine Library
x-ai-rulez:
  kind: context
  id: engine-library
  metadata:
    priority: high
---

## `crates/poly-core/` — the engine library (lib `poly_core`; path dep; not published)

`src/`:

- `lib.rs` — public re-exports.
- `engine.rs` — re-exports `poly-engine-api`'s `engine`: the **`Engine` trait** contract plus `SourceFile`, `Capabilities`,
  `Severity`, `Span`, `Edit`, `Diagnostic`, `FormatOutput`. This is the keystone abstraction.
- `registry.rs` — crate-private `engines_for(&Language) -> Vec<Box<dyn Engine>>`. A static
  `match` (alef-style): the native crate backend(s) registered for the language, else the
  tree-sitter generic backend — then the four **cross-cutting** backends (`typos`, `astgrep`,
  `uncomment`, `quality`) appended for every language. `runner/plan.rs` plans `typos` only when config enables it (`registry::OPT_IN_ENGINES`)
  and merges enabled catalog tools into that list.
- `config.rs` — normalization and per-engine config slices of the schema the `poly-config`
  crate parses. The config file is `poly.toml` and nothing else (`CONFIG_FILE_NAMES` has one
  entry; there is no YAML form), read with the `toml` crate — `toml_edit` appears only in
  `poly migrate`, which *writes* into an existing `poly.toml` without disturbing comments. v2
  `[engines.<id>]` tables are resolved by `config/engine_first.rs` (v1 `lint`/`fmt` tables still
  load; mixing is rejected).
  `poly.local.toml` layers local overrides on top. A top-level `extends` list shares config
  from local or pinned-remote base configs (ADR 0020): bases deep-merge beneath the declaring
  file, `poly.local.toml` stays the final layer, and `poly lock` pins a symbolic git
  ref into `poly.lock`. Schema + the network-free `BaseConfigResolver` trait live in the
  `poly-config` crate (`extends.rs`); the git fetch lives in the `poly-cli` `remote` module.
- `defaults.rs` — `normalize_whitespace`, the shared final pass any formatter can reuse (line
  endings, trailing whitespace, trailing blank lines, single final newline). The opinionated
  values themselves are `poly_config::GlobalDefaults` (`line_length = 120`, `line_ending = lf`,
  `final_newline`, `trim_trailing_whitespace`), re-exported through `config.rs`. Layering is **tool default → opinionated override → user `poly.toml`**.
- `resolve.rs` — hierarchical, monorepo-aware config resolution (ADR 0018): discovers every
  in-tree `poly.toml` and maps each discovered file to the nearest config governing it.
- `fingerprint.rs` — `ConfigFingerprint`, a stable digest of the configuration governing a run,
  one per directory-scoped config. Feeds `LintDocument`/`FormatDocument`'s
  `configs` list, so two clean reports are only comparable when their fingerprints match.
- `discover.rs` — file walk via the `ignore` crate (respects `.gitignore`).
- `runner.rs` + `runner/` — the pipeline: discover → cache → engine → report, parallelized with
  **rayon `par_iter` over files**. Split by concern: `runner/plan.rs` (per-language engine plan,
  catalog merge, `provides_language_lint`), `runner/edits.rs` (atomic autofix application),
  `runner/skips.rs` (`SkippedFile` and its reasons — `NO_ENGINE_SKIP`, `NO_LINT_RULES_SKIP_PREFIX`,
  `FILTERED_SKIP` / `DISABLED_SKIP_PREFIX` for `--only`/`--skip`/`enabled = false`,
  `GENERATED_SKIP`, `BINARY_SKIP`), `runner/selection.rs` (`--only`/`--skip`: narrows an
  already-planned engine set and never enables one that config left off; rejects an unknown
  engine name before the run starts), `runner/generated.rs` (whether the run acts on
  machine-generated files at all vs. whether it may rewrite one it did check),
  `runner/types.rs` (`LintRun`/`FormatRun`/`RunOptions` and friends), and `runner/tests.rs`.
- `components/` — which engine components a run needs (`needed.rs`), the install/`on_missing`
  policy applied before planning (`preplan.rs`), and the component-host seam (`seam.rs`). An
  engine family whose `engine-*` feature is off is served by a downloaded component.
- `filter/` — result and discovery filtering, one question per file: `diagnostics.rs`
  (`PerFileIgnores`, `SeverityRemap`), `paths.rs`, `generated.rs` (generated-source detection),
  `suppress.rs` (in-source `poly: allow[…]` directives).
- `report/` — three formats: `pretty`, `json` and `toon`. `report/shared.rs` holds `Verbosity`, `notes.rs` the
  discovery/skip notes, `lint.rs`/`format.rs` the human renderers, `structured.rs` the
  machine-readable ones, `document.rs` the `LintDocument`/`FormatDocument` object shape
  (`results` + `errors` + `skipped` + `summary` + `configs`) both renderers serialize,
  `render.rs` the `RenderError` a failed serialization must surface rather than emit an empty
  document, `theme.rs` the palette, `layout.rs` shared typography.
- `language.rs` — `Language` enum + detection **by filename and extension**; an unrecognized
  extension becomes `Language::Other(name)`, and it is the tier-2 engine — not this module —
  that hands that name to `tree-sitter-language-pack`.
- `engines/` — one module per backend. The python (`ruff`), php (`mago`), sql (`sqruff`), nix
  (`nixfmt`), `typos`, `astgrep`, `uncomment` and misc (`dockerfile`, `dotenv`, `hcl`, `graphql`)
  backends now live in `crates/poly-engines-<family>` and are re-exported here at their
  old paths (`engines/mod.rs`). Most of the rest are a single `engines/<tool>.rs`; a backend that
  outgrows the 1000-line cap becomes a directory split per concern (`oxc/`,
  `native_tool/`, `quality/`, `treesitter/`, `catalog_tool/`; in the family crates `mago/`, `hcl/`
  and `ruff/`, whose `ruff/` submodules sit beside `ruff.rs`):
  - tier-1 native crate backends, registered per language in `registry.rs`: `ruff.rs` +
    `ruff/` (Python, in `poly-engines-python`; target version: `poly-engine-api`'s
    `host_hooks/ruff_target.rs`), `oxc/` (JS/TS/JSX/TSX/JSON/JSONC), `taplo.rs` (TOML: taplo
    lint, `oxc_formatter_toml` format unless a taplo layout key selects taplo's formatter),
    `rumdl.rs` (Markdown/MDX lint; `markdown_fmt.rs` formats through `oxc_formatter_markdown`
    with `markdown_embed.rs` dispatching fences and front matter), `adrlint.rs` (opt-in MADR rules
    for numbered ADRs — document rules only, see ADR 0037), `sqruff.rs` (SQL, in
    `poly-engines-sql`), `yaml.rs`
    (`serde_yaml` lint, `oxc_formatter_yaml` format), `malva.rs` (CSS/SCSS/Less — formats via
    `oxc_formatter_css` unless a malva key is set), `biome_css.rs` and `biome_graphql.rs` (both
    named `"biome"`; shared helpers in `biome_common.rs`), `graphql.rs`, `markup_fmt.rs`
    (HTML/Vue/Svelte/Astro/Angular/Jinja/Vento/Mustache/XML), `mago/` (PHP, in `poly-engines-php`),
    `nixfmt.rs` (Nix, in `poly-engines-nix` — named `"alejandra"`, the formatter it wraps),
    `rubyfmt` (Ruby, in `components/crates/poly-engines-ruby`, outside the root workspace), `hcl/` (`hcl-edit` lint plus a port of `hclwrite.Format`),
    `dockerfile.rs` (vendored `dockerfile-parser`), `dotenv.rs` (with `graphql.rs`, in `poly-engines-misc`),
    `ini.rs`. A project `.oxfmtrc.json[c]` / `.oxlintrc.json[c]` is read by
    `poly-engine-api`'s `host_hooks/oxfmt_native.rs` / `oxfmt_scope.rs` and `poly-config`'s `oxlint_native.rs`.
  - **cross-cutting backends** (`languages() == &[]`, appended by the registry to every
    language): `typos.rs` (spell-check, opt-in; `poly-engines-typos`), `poly-engines-astgrep` (ast-grep
    rules — user rule dirs plus the built-in pack in `src/builtin/`, see Coverage tiers),
    `uncomment.rs` (`poly-engines-uncomment`), and `quality/` (ADR 0027 code-quality metrics).
  - `uncomment.rs` (in `poly-engines-uncomment`) — the **opt-in comment-removal lint backend** wrapping the `uncomment`
    crate: reports each removable comment as a warning with a delete-edit, gated on
    `[engines.uncomment] enabled = true` (or per-language `[engines.uncomment.lang.<lang>]`).
  - `treesitter/` — the **tier-2 generic formatter** built on `tree-sitter-language-pack`:
    CST-driven structural reindent for brace grammars, whitespace normalization
    (`defaults.rs::normalize_whitespace`) otherwise, and leave-untouched for grammars where
    whitespace is significant. Parsers are pooled thread-locally, never built per file.
  - `native_tool/` — the **native-toolchain backend** (table-driven, `spec.rs`): wraps a
    language's canonical first-party CLI as a stdin→stdout subprocess when present and
    enabled (or batched over many files, opt-in per tool with `batch`, ADR 0033). Eleven specs
    today — `gofmt`, `rustfmt`, `shfmt` and `shellcheck` (default-on), plus `zig fmt`,
    `google-java-format`, `ktfmt`, `Rscript`/styler, `swift-format`, `dart format`,
    `gleam format` (all opt-in). One directory, one table — not one file per tool.
  - `catalog_tool/` — the opt-in **catalog tier** (ADR 0013): runs any `poly-catalog` tool a
    user enables with `[tools.<name>] enabled = true`, over stdin or a temp file. Capability-
    probed, so a missing binary is a warning rather than an error. `catalog_tool/recipe/` is
    the **recipe tier** (ADR 0035): fourteen tools (`golangci-lint`, `helm`, `buf`, `hadolint`,
    `tflint`, `checkstyle`, …) whose overlay entry in `poly-catalog`'s `overlay.json` carries a
    recipe run batched per unit (module, chart, directory), with their report parsed into
    per-line diagnostics and an optional pinned, checksummed download (`install = "auto"`).
  - support modules, not backends: `biome_common.rs`, `rule_config.rs` (re-exported from
    `poly-engine-api`; the shared
    `select` / `extend_select` / `ignore` / `[engines.<tool>.rules.<id>]` schema),
    `config_keys/` (each engine's declared option keys, behind `unknown-config-key` and the
    published JSON Schema), `core_options.rs` (layout options shared by the oxfmt-equivalent
    formatters), `format_scope.rs` (hands named files such as `pyproject.toml` to a more specific
    formatter), `line_index.rs` (offset-to-line index; lives in `poly-engine-api`, the shared grammar
    memo in `poly-grammar`),
    `template.rs` (also `poly-engine-api`; Go/Helm
    template detection behind the `yaml` and `rumdl` `skip_reason`s; `markup_fmt` has its own
    template-target check).

Result caching is **not** in this crate: it lives in `crates/poly-cache` (`ResultCache`), a
blake3 content-hash cache whose key is `blake3(cache format version \0 build identity \0
namespace \0 engine name \0 engine version \0 toml(resolved engine config) \0 input digest)`.
The cache lives in the
per-user OS cache dir (`~/.cache/poly/<repo-key>`, `~/Library/Caches/poly/…`,
`%LOCALAPPDATA%\poly\…`) resolved via `etcetera`, overridable with `POLY_CACHE_HOME` or pinned
via `[cache] dir`; `--no-cache` bypasses.

