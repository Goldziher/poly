---
type: Concept
title: Runtime Architecture
x-ai-rulez:
  kind: context
  id: runtime-architecture
  metadata:
    priority: high
---

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
    fn option_keys(&self, table: OptionTable) -> OptionKeys;      // declared [lint]/[fmt] keys
    fn provides_language_lint(&self, language: &Language, cfg: &EngineConfig) -> bool;
    fn skip_reason(&self, src: &SourceFile) -> Option<&'static str>;
    fn lint(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>>;
    fn format(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput>;
    fn self_manages_enabled(&self) -> bool;
    fn supersedes_generic_formatter(&self) -> bool;
    // ...plus cache salts (`*_context`, `*_salt`), batching (`*_batch`) and catalog takeover.
}
pub enum FormatOutput { Unchanged, Formatted(String) }
```

Contract rules: `Engine` is `Send + Sync` (it runs inside a rayon `par_iter`); `version()`
must change whenever output could change, because it is part of the cache key (other inputs,
such as a project config file, go in through the salt/context methods); `format`
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

The four defaulted predicates carry the coverage, skip and enablement accounting:

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
- `self_manages_enabled()` — whether this backend reads `enabled` itself and degrades
  gracefully with it `false`, rather than expecting the runner to drop it from the plan before
  the file loop. `false` (the default) is right for a backend whose absence means one less
  check; only `native_tool` answers `true` — it is its language's only registry slot and hands
  the file to the tier-2 reindenter itself when disabled, so dropping it from the plan instead
  would leave that language with no formatter at all.
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

   - **`rustfmt`, `gofmt`, `shellcheck` and `shfmt` are default-on when present; everything
     else is opt-in.** `rustfmt`/`gofmt` per ADR 0014's 2026-06-28 amendment, the shell pair per
     the 2026-08-29 / 2026-09-15 ones (shell has no first-party tool); the rest stay opt-in via
     `[fmt.<lang>.<tool>] enabled = true`. Output then depends on the host tool's presence and
     version, which is at odds with reproducibility — so CI must pin the toolchain. Either way,
     when the tool is absent the language falls through to tier-2, so the zero-dependency promise
     is intact for anyone without the toolchain installed.
   - **Capability-gated, graceful degradation.** Probe for the tool once (cached); declare the
     `format`/`lint` capability only when it is found *and* enabled; otherwise the language
     falls through to tier-2. A missing toolchain is never an error — just lower fidelity.
   - **Per-file, stdin→stdout.** Wrap only tools that process a single file over
     stdin/stdout (`gofmt`, `rustfmt`, `zig fmt`); opt-in `batch = true` (ADR 0033) must match
     the per-file output exactly. Project-wide tools that must compile a
     package — `clippy`, `go vet`, `mix format` — do **not** fit the rayon per-file unit or the
     content-hash cache and are explicitly out of scope for this tier.
   - **Honest cache key + least privilege.** Fold the tool's resolved `--version` into
     `version()` so a toolchain upgrade invalidates the cache. Invoke with a fixed argv, no
     shell, content fed on stdin — never pass file contents through a shell.
4. **Catalog tier** (`catalog_tool/`, ADR 0013) — any `poly-catalog` tool a user opts into with
   `[tools.<name>] enabled = true`, run per file over stdin or a temp file. Breadth over
   fidelity: a lint failure maps to one file-level `Diagnostic` with no span and no rule code.
   An absent binary is a warning, not an error. **Recipe tools** (`catalog_tool/recipe/`, ADR
   0035; `golangci-lint`, `helm`, `buf`, …) run batched per unit with parsed per-line reports and
   optional pinned downloads (`install = "auto"`); a whole-language one takes over tier-1.

### Cross-cutting backends (every language)

The registry appends four backends with `languages() == &[]` to every language, so they are
orthogonal to the tiers above rather than a step in the resolution order: `typos` (spell-check,
warning, on by default), `astgrep`, `uncomment` (opt-in), and `quality`.

Two of them carry poly's lint coverage for languages no tier-1 backend reaches:

- **`quality/` — the code-quality tier (ADR 0027).** Tree-sitter metric rules at *warning*
  severity, on by default: `file-too-long` (1000), `function-too-long` (80), `type-too-long`
  (300), `too-many-parameters` (6), `nesting-too-deep` (4), `cyclomatic-complexity` (20),
  `lazy-ignore`; `magic-number` and `law-of-demeter` ship opt-in; path-only
  `uncollected-test-file` (ADR 0036) is on for Python, Go and Ruby. A **per-language deferral
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
  `dirs` and `builtin`; the per-rule override table is nested inside the engine's own table.) A
  rule whose findings concentrate in generated or test code declares its own default path
  exclusions with the standard ast-grep `ignores:` key in its YAML (four rules do today);
  `engines/astgrep/exclusions.rs` collects them per config and the runner merges them into
  `PerFileIgnores` as **defaults** — a `[per-file-ignores]` entry naming the rule replaces them
  outright. Every finding they drop appears in the run's `suppressed` list.

Both answer `provides_language_lint` from the same lookup their `lint` performs, never from "the
engine is switched on".

## Tests

- `crates/poly-core/tests/pipeline.rs` — end-to-end pipeline contract.
- Per-backend `insta` fixtures: a known-bad file (expected `Diagnostic`s) and a
  known-unformatted file (exact formatted output).
