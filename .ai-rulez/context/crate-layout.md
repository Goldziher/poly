---
priority: high
---

# Crate Layout

poly is a Cargo **workspace** that ships a single self-contained binary, `poly`, driven by one
config. Its subcommands cover the whole surface: `poly lint`, `poly fmt`, `poly hooks`, `poly
commit`, `poly rules`, `poly cache`, `poly config`, `poly migrate`, `poly mcp`, and `poly
doctor`. Everything runs **in-process, pure Rust** — no subprocess, no system dependency by
default (three scoped exceptions: **native-toolchain backends**, the opt-in **catalog-tool**
tier, and the `poly hooks` engine — see Coverage tiers below).

A tool is consumed as a crate dependency, **from crates.io whenever a usable version is
published**. That includes ruff, which moved off its git pin on 2026-08-29: `ruff_linter =
"=0.16.5"` and `ruff_db` / `ruff_formatter` / `ruff_python_ast` / `ruff_python_formatter` /
`ruff_text_size` = `"=0.0.11"`. The `=` is deliberate — these are unsemvered internals, and an
exact pin preserves the property the git `rev` had. The only remaining **pinned git `rev`** deps
are **oxc**, **biome**, and **rubyfmt**, each for a reason recorded in ADR 0003's 2026-08-29
amendment: four of oxc's crates are `publish = false` and a partial migration is a hard compile
error (a registry `SourceType` will not typecheck against the git `oxc_formatter`); biome's
published crates ship 2024 code under today's version numbers; the crates.io `rubyfmt` is a
`0.0.0-wip` name reservation. A git pin records upstream's publishing posture at a moment in
time, not a permanent property — re-check the pins when touching dependencies. We do **not**
vendor and we do **not** publish our own crates to crates.io — the binary is distributed as
prebuilt release artifacts plus an installer (see release-versioning).

## Workspace root

- `Cargo.toml` — `[workspace]` (resolver 3, edition 2024). Eleven members: `poly-core` (the
  engine library), `poly-cli` (builds the `poly` binary), `poly-config`, `poly-cache`,
  `poly-catalog`, `poly-hooks`, `poly-workspace`, `poly-mcp`, `poly-buildinfo`, and two that
  do **not** carry the prefix — `gitfluff` (the commit-message linter behind `poly commit`) and
  `conformance` (a dev-only differential formatter-conformance harness the shipped binary never
  depends on). Shared deps live under `[workspace.dependencies]`; git deps are pinned to a `rev`
  (crates from one monorepo share one `rev`).
- `deny.toml` — `cargo deny` license / source allow-list (no GPL/AGPL), applied across the full
  dependency tree including git deps and their transitive dependencies.
- `crates/poly-workspace/` — the whole-project (workspace) lint orchestration shared by
  `poly-cli` and `poly-mcp`: lowers `[hooks]` config into the `poly-hooks` model, reduces it to
  the whole-project tool set (`cargo clippy`/`cargo-sort`/`cargo-machete`/`cargo-deny`), and runs
  it against the live worktree. Public API is narrow —
  `run_workspace_lint(&PolyConfig, &WorkspaceLintOptions) -> WorkspaceLintOutcome` plus
  `render_workspace_outcome` — so both consumers get identical behavior without a dependency
  cycle: the crate never loads config itself, the caller injects an already-resolved
  `poly_config::PolyConfig` (the CLI keeps its git-remote `extends` resolver, the MCP server
  keeps its network-free one).
- `.ai-rulez/`, `.claude-plugin/`, `.codex-plugin/` — the generated plugin/marketplace surface
  (ADR 0022): hand-written source lives under `.ai-rulez/rules/`, `.ai-rulez/context/`,
  `.ai-rulez/skills/`, `.ai-rulez/commands/`, `.ai-rulez/agents/`, and `.ai-rulez/config.toml`;
  the generated `CLAUDE.md` / `AGENTS.md`, `.claude-plugin/*.json`, and
  `.codex-plugin/plugin.json` are output and must never be hand-edited.
  `scripts/release-bump.sh <version>` bumps the workspace and plugin versions together,
  regenerates the plugin outputs, and asserts the generated manifests match before finishing.

## `crates/poly-core/` — the engine library (lib `poly_core`; path dep; not published)

`src/`:

- `lib.rs` — public re-exports.
- `engine.rs` — the **`Engine` trait** contract plus `SourceFile`, `Capabilities`,
  `Severity`, `Span`, `Edit`, `Diagnostic`, `FormatOutput`. This is the keystone abstraction.
- `registry.rs` — crate-private `engines_for(&Language) -> Vec<Box<dyn Engine>>`. A static
  `match` (alef-style): the native crate backend(s) registered for the language, else the
  tree-sitter generic backend — then the four **cross-cutting** backends (`typos`, `astgrep`,
  `uncomment`, `quality`) appended for every language. `runner/plan.rs` merges enabled catalog
  tools into that list.
- `config.rs` — normalization and per-engine config slices of the schema the `poly-config`
  crate parses. The config file is `poly.toml` and nothing else (`CONFIG_FILE_NAMES` has one
  entry; there is no YAML form), read with the `toml` crate — `toml_edit` appears only in
  `poly migrate`, which *writes* into an existing `poly.toml` without disturbing comments.
  `poly.local.toml` layers local overrides on top. A top-level `extends` list shares config
  from local or pinned-remote base configs (ADR 0020): bases deep-merge beneath the declaring
  file, `poly.local.toml` stays the final layer, and `poly config update` locks a symbolic git
  ref into `poly-config.lock`. Schema + the network-free `BaseConfigResolver` trait live in the
  `poly-config` crate (`extends.rs`); the git fetch lives in the `poly-cli` `remote` module.
- `defaults.rs` — `normalize_whitespace`, the shared final pass any formatter can reuse (line
  endings, trailing whitespace, trailing blank lines, single final newline). The opinionated
  values themselves are `poly_config::GlobalDefaults` (`line_length = 120`, `line_ending = lf`,
  `final_newline`, `trim_trailing_whitespace`), re-exported through `config.rs`; per-tool
  opinions such as "always format docstrings" live in the engine that owns the setting.
  Layering is **tool default → opinionated override → user `poly.toml`**.
- `resolve.rs` — hierarchical, monorepo-aware config resolution (ADR 0018): discovers every
  in-tree `poly.toml` and maps each discovered file to the nearest config governing it.
- `discover.rs` — file walk via the `ignore` crate (respects `.gitignore`).
- `runner.rs` + `runner/` — the pipeline: discover → cache → engine → report, parallelized with
  **rayon `par_iter` over files**. Split by concern: `runner/plan.rs` (per-language engine plan,
  catalog merge, `provides_language_lint`), `runner/edits.rs` (atomic autofix application),
  `runner/skips.rs` (`SkippedFile`, `NO_ENGINE_SKIP`, `NO_LINT_RULES_SKIP_PREFIX`),
  `runner/types.rs` (`LintRun`/`FormatRun`/`RunOptions` and friends).
- `filter/` — result and discovery filtering, one question per file: `diagnostics.rs`
  (`PerFileIgnores`, `SeverityRemap`), `paths.rs`, `generated.rs` (generated-source detection),
  `suppress.rs` (in-source `poly: allow[…]` directives).
- `report/` — three formats: `pretty` (colored via `owo-colors`' `if_supports_color`), `json`
  (`serde_json`), and `toon`. `report/shared.rs` holds `Verbosity`, `notes.rs` the
  discovery/skip notes, `lint.rs`/`format.rs` the human renderers, `structured.rs` the
  machine-readable ones, `render.rs` the `RenderError` a failed serialization must surface
  rather than emit an empty document.
- `language.rs` — `Language` enum + detection **by filename and extension**; an unrecognized
  extension becomes `Language::Other(name)`, and it is the tier-2 engine — not this module —
  that hands that name to `tree-sitter-language-pack`.
- `engines/` — one module per backend. Most are a single `engines/<tool>.rs`; a backend that
  outgrows the 1000-line cap becomes a directory split per concern (`oxc/`, `mago/`,
  `native_tool/`, `astgrep/`, `quality/`, `treesitter/`, `catalog_tool/`):
  - tier-1 native crate backends, registered per language in `registry.rs`: `ruff.rs`
    (Python), `oxc/` (JS/TS/JSX/TSX/JSON/JSONC), `taplo.rs` (TOML), `rumdl.rs`
    (Markdown/MDX), `sqruff.rs` (SQL), `yaml.rs`, `malva.rs` (CSS/SCSS/Less), `biome_css.rs`
    and `biome_graphql.rs` (both named `"biome"`; shared helpers in `biome_common.rs`),
    `graphql.rs`, `markup_fmt.rs` (HTML/Vue/Svelte/Astro/Angular/Jinja/Vento/Mustache/XML),
    `mago/` (PHP), `nixfmt.rs` (Nix — named `"alejandra"`, the formatter it wraps),
    `rubyfmt.rs` (Ruby), `hcl.rs`, `dockerfile.rs`, `dotenv.rs`, `ini.rs`.
  - **cross-cutting backends** (`languages() == &[]`, appended by the registry to every
    language): `typos.rs` (spell-check), `astgrep/` (ast-grep rules — user rule dirs plus the
    built-in pack, see Coverage tiers), `uncomment.rs`, and `quality/` (ADR 0027 code-quality
    metrics).
  - `uncomment.rs` — the **opt-in comment-removal lint backend** wrapping the `uncomment`
    crate: reports each removable comment as a warning with a delete-edit, gated on
    `[lint.uncomment] enabled = true` (or per-language `[lint.<lang>.uncomment]`).
  - `treesitter/` — the **tier-2 generic formatter** built on `tree-sitter-language-pack`:
    CST-driven structural reindent for brace grammars, whitespace normalization
    (`defaults.rs::normalize_whitespace`) otherwise, and leave-untouched for grammars where
    whitespace is significant. Parsers are pooled thread-locally, never built per file.
  - `native_tool/` — the **native-toolchain backend** (table-driven, `spec.rs`): wraps a
    language's canonical first-party CLI as a stdin→stdout subprocess when present and
    enabled. Eleven specs today — `gofmt` and `rustfmt` (default-on), plus `zig fmt`, `shfmt`,
    `shellcheck`, `google-java-format`, `ktfmt`, `Rscript`/styler, `swift-format`,
    `dart format`, `gleam format` (all opt-in). One directory, one table — not one file per
    tool.
  - `catalog_tool/` — the opt-in **catalog tier** (ADR 0013): runs any `poly-catalog` tool a
    user enables with `[tools.<name>] enabled = true`, over stdin or a temp file. Capability-
    probed, so a missing binary is a no-op rather than an error.
  - support modules, not backends: `biome_common.rs`, `rule_config.rs` (the shared
    `select` / `extend_select` / `ignore` / `[lint.<lang>.<tool>.rules.<id>]` schema),
    `template.rs` (Go/Helm
    template detection behind the `yaml` and `rumdl` `skip_reason`s; `markup_fmt` has its own
    template-target check).

Result caching is **not** in this crate: it lives in `crates/poly-cache` (`ResultCache`), a
blake3 content-hash cache whose key is `blake3(cache format version \0 build identity \0
namespace \0 engine name \0 engine version \0 toml(resolved engine config) \0 input digest)`.
`get`/`put` take no lock and rely on atomic sibling-tmp-then-rename. The cache lives in the
per-user OS cache dir (`~/.cache/poly/<repo-key>`, `~/Library/Caches/poly/…`,
`%LOCALAPPDATA%\poly\…`) resolved via `etcetera`, overridable with `POLY_CACHE_HOME` or pinned
via `[cache] dir`; `--no-cache` bypasses.

## The `poly` binary (thin CLI)

`crates/poly-cli` is a thin clap wrapper over `poly-core`. Lint and format are subcommands:
`poly lint [PATHS]… --fix --format pretty|json|toon --config <p> --no-cache -j <N> --no-color
--exclude <GLOB> --verbose --debug --deny-skips|--max-skips <N> --no-workspace|--workspace`;
`poly fmt [PATHS]… --check …` (dry-run is the default; `--check` is the explicit form). A
consuming repo collapses its hook sprawl onto poly's own `poly hooks`
runner via `poly.toml [hooks]` (ADR 0012) — poly no longer ships a `.pre-commit-hooks.yaml`, so
there is no external pre-commit-framework dependency in between.

## The `Engine` trait contract (`engine.rs`)

Every backend — native crate or generic tier — implements the same trait:

```rust
pub trait Engine: Send + Sync {
    fn name(&self) -> &'static str;                    // the *tool* id, not the Rust type
    fn languages(&self) -> &'static [Language];        // empty == cross-cutting
    fn capabilities(&self) -> Capabilities;            // lint / format / fix
    fn version(&self) -> &str;                         // folded into the cache key

    // Defaulted methods:
    fn provides_language_lint(&self, language: &Language, cfg: &EngineConfig) -> bool;
    fn skip_reason(&self, src: &SourceFile) -> Option<&'static str>;
    fn lint(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>>;
    fn format(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput>;
    fn supersedes_generic_formatter(&self) -> bool;
}
pub enum FormatOutput { Unchanged, Formatted(String) }
```

Contract rules: `Engine` is `Send + Sync` (it runs inside a rayon `par_iter`); `version()`
must change whenever output could change, because it is part of the cache key; `format`
returns `Unchanged` rather than echoing input so the runner can skip writes; `lint`/`format`
default to no-ops for engines that lack a capability (declare honestly via `capabilities()`).
The trait is an **internal extension point**, not stable public API — backends live in
`poly-core` and are reached through `poly_core::lint` / `poly_core::format`. `SourceFile.content`
is an `Arc<str>`, so one file's bytes are shared across every engine that runs on it (and across
fix passes) rather than re-cloned on the per-file hot path.

`name()` names the **tool a user configures**, not the Rust type, and is unique *per language*
rather than globally: it keys `[<kind>.<lang>.<engine>]`, the `engine` field of every
`Diagnostic`, and the cache key. Hence `NixFmtEngine` is `"alejandra"`, and `BiomeCssEngine` /
`BiomeGraphqlEngine` are both `"biome"`.

The three defaulted predicates carry the coverage and skip accounting:

- `provides_language_lint(language, cfg)` — "did anything in this run know how to lint this
  file?" It drives the `no lint rules for <language>` skip, the `checked` count, the JSON
  `skipped` payload, and `--deny-skips`. The default answers from `languages()`: a backend
  registered for languages lints them; a cross-cutting one claims nothing. Backends whose
  answer depends on the host or config override it — `native_tool` requires the binary to be
  installed *and* enabled, `astgrep` runs the same rule lookup `lint` does, `quality` claims a
  language only where it can model that language's control flow. A `true` here is a claim the
  run relies on; never make it speculatively.
- `skip_reason(src)` — why a backend declines a specific file (templated YAML is not YAML).
  Returning the reason, rather than bailing out inside `lint`/`format`, is what lets the runner
  count and report the skip instead of silently passing the file off as checked.
- `supersedes_generic_formatter()` — whether a configured, runnable formatter should displace
  the tier-2 reindenter, so the two do not fight over indentation and prevent convergence.

## Coverage tiers

Per-language resolution order:

1. **Native Rust crate backend (tier-1)** where one exists — highest-fidelity, fully
   in-process, zero deps; registered for its specific languages in `registry.rs`.
2. **Tree-sitter generic tier (tier-2)** (`treesitter/`) — the catch-all for *everything else*
   (Java, Elixir, C/C++, protobuf, and the long tail of 300+ grammars). Best-effort structural
   reindent, pure Rust, grammars fetched on demand → still zero system deps. This is the
   coverage mechanism, not a fallback to avoid; native ports can later upgrade individual
   languages from tier-2 to tier-1 fidelity. Tier-2 is **formatting only** — it establishes no
   lint coverage, which is why a tier-2 language reports `no lint rules for <language>` unless
   a cross-cutting backend claims it.
3. **Native-toolchain backend** (`native_tool/`) — for languages whose *canonical*
   formatter/linter is a first-party CLI with no usable Rust library: Go's `gofmt`, Rust's
   `rustfmt`, Zig's `zig fmt`, and the like. This is the **primary scoped exception** to the
   no-subprocess rule (the opt-in catalog tier and the `poly hooks` engine are the other two,
   separate ones). It exists because no
   pure-Rust crate can match these tools, and reimplementing them is a disproportionate
   maintenance sink. Strict discipline keeps the exception honest:

   - **`rustfmt` and `gofmt` are default-on when present; everything else is opt-in, off by
     default.** The two canonical formatters with no viable Rust library run automatically when
     found on PATH (ADR 0014 amendment, 2026-06-28), matching what `cargo fmt`/`gofmt` users
     already expect; `zig fmt` and every native *lint* tool stay opt-in via config
     (`[fmt.<lang>.<tool>] enabled = true`). Output then depends on the host tool's presence and
     version, which is at odds with reproducibility — so CI must pin the toolchain. Either way,
     when the tool is absent the language falls through to tier-2, so the zero-dependency promise
     is intact for anyone without the toolchain installed.
   - **Capability-gated, graceful degradation.** Probe for the tool once (cached); declare the
     `format`/`lint` capability only when it is found *and* enabled; otherwise the language
     falls through to tier-2. A missing toolchain is never an error — just lower fidelity.
   - **Per-file, stdin→stdout only.** Wrap only tools that process a single file over
     stdin/stdout (`gofmt`, `rustfmt`, `zig fmt`). Project-wide tools that must compile a
     package — `clippy`, `go vet`, `mix format` — do **not** fit the rayon per-file unit or the
     content-hash cache and are explicitly out of scope for this tier.
   - **Honest cache key + least privilege.** Fold the tool's resolved `--version` into
     `version()` so a toolchain upgrade invalidates the cache. Invoke with a fixed argv, no
     shell, content fed on stdin — never pass file contents through a shell.
4. **Catalog tier** (`catalog_tool/`, ADR 0013) — any `poly-catalog` tool a user opts into with
   `[tools.<name>] enabled = true`, run per file over stdin or a temp file. Breadth over
   fidelity: a lint failure maps to one file-level `Diagnostic` with no span and no rule code.
   Capability-probed, so an absent binary is a no-op.

### Cross-cutting backends (every language)

The registry appends four backends with `languages() == &[]` to every language, so they are
orthogonal to the tiers above rather than a step in the resolution order: `typos` (spell-check,
warning, on by default), `astgrep`, `uncomment` (opt-in), and `quality`.

Two of them carry poly's lint coverage for languages no tier-1 backend reaches:

- **`quality/` — the code-quality tier (ADR 0027).** Tree-sitter metric rules at *warning*
  severity, on by default: `file-too-long` (1000), `function-too-long` (80), `type-too-long`
  (300), `too-many-parameters` (6), `nesting-too-deep` (4), `cyclomatic-complexity` (20),
  `lazy-ignore`; `magic-number` and `law-of-demeter` ship opt-in. A **per-language deferral
  table** (`quality/family.rs`) keeps the tier from ever duplicating a tier-1 rule (ruff's
  `C901`/`PLR0913`, oxlint's `max-depth`/`max-params`). Coverage is claimed honestly: only where
  poly has both a definition query (`definitions::has_query`) **and** a construct table
  (`kinds::has_table`) — Python, Rust, Go, JavaScript, TypeScript, TSX, Java, Kotlin, C, C++,
  C#, Ruby. Query-only grammars (Zig, Swift, Dart, Gleam, Elixir, PHP, Nix, Scala, Lua, R) still
  *report* the line-count floor but do **not** count as linted.
- **`astgrep/builtin/` — the built-in ast-grep rule pack (ADR 0029).** 26 rules across 9
  languages (C#, Elixir, Go, Java, Kotlin, Python, Ruby, Rust, Swift), each a YAML file embedded
  with `include_str!` and compiled once into a `OnceLock`. Pack rules go through the same
  `ast_grep_config::from_yaml_string` path as user rules and sit **beneath** them — a user rule
  with the same `id` replaces the pack's outright. Each rule authors its own default `severity:`,
  including `off`; 13 of the 26 ship `off`. `[lint.astgrep]` `select`/`extend_select`/`ignore`
  and `[lint.astgrep.rules.<id>] level` move any of them; the *top-level* `[rules] builtin =
  false` disables the pack wholesale. (Two tables named `rules`: top-level `[rules]` holds
  `dirs` and `builtin`; the per-rule override table is nested inside the engine's own table.) A hardcoded `NOISY_PATH_EXCLUSIONS` table (four rule ids today) is a stand-in for
  engine-supplied `[per-file-ignores]` defaults, which do not exist yet.

Both answer `provides_language_lint` from the same lookup their `lint` performs, never from "the
engine is switched on".

## Tests

- `crates/poly-core/tests/pipeline.rs` — end-to-end pipeline contract.
- Per-backend `insta` fixtures: a known-bad file (expected `Diagnostic`s) and a
  known-unformatted file (exact formatted output).
