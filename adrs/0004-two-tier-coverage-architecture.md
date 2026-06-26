# 0004 — Two-Tier Coverage Architecture

- Status: Accepted
- Date: 2026-06-26

## Context

v1 must cover the entire xberg-io language set (ADR 0001) with zero system dependencies
(ADR 0002). High-quality native Rust crates exist for _some_ languages, but not for the
long tail (shell, Go, Java, Kotlin, Ruby, PHP, Elixir, C/C++, Dockerfile, proto, Rust, and
300+ more). We need a single architecture that delivers full coverage today while leaving
room to raise fidelity language-by-language over time.

## Decision

All backends implement one `Engine` trait and are resolved through a static registry.
Coverage comes from exactly two tiers — nothing else:

- **Tier-1: native Rust crate backends.** Where a quality crate exists (ADR 0005), a thin
  adapter implements `Engine` and is registered for that language. These give idiomatic,
  high-fidelity lint/format.
- **Tier-2: tree-sitter generic formatter.** Built on `tree-sitter-language-pack`
  (`get_language` / `detect_language`, on-demand grammar download). For every language
  _without_ a native backend, it parses the CST and re-emits source via structural
  reindentation and whitespace normalization (consistent indent unit, trim trailing
  whitespace, collapse blank-line runs, final newline, normalize line endings).
  **Tier-2 is the coverage mechanism, not a fallback we hope to avoid** — it is what makes
  "universal, zero-dependency" true on day one.

```rust
pub trait Engine: Send + Sync {
    fn name(&self) -> &'static str;
    fn languages(&self) -> &[Language];
    fn capabilities(&self) -> Capabilities;     // lint / format / fix
    fn version(&self) -> &str;                   // folded into cache key
    fn lint(&self, src: &SourceFile, cfg: &EngineConfig) -> Result<Vec<Diagnostic>>;
    fn format(&self, src: &SourceFile, cfg: &EngineConfig) -> Result<FormatOutput>;
}
```

Resolution per language is a static `match` (alef-style `try_get_backend`): native backend
if registered, else the tree-sitter generic backend. Adding a native backend is a one-line
registry change that automatically upgrades that language from tier-2 to tier-1.

## Consequences

Positive:

- Full v1 coverage with one uniform abstraction; the runner, cache, and reporter never
  special-case a language.
- Clear, incremental upgrade path: port a language to tier-1 without touching the
  pipeline.
- `version()` in the trait feeds the cache key (ADR 0008), so upgrades invalidate cleanly.

Negative / risks:

- **Tier-2 output is best-effort, not idiomatic per-language reflow.** It reindents and
  normalizes whitespace; it does not match what `gofmt`/`rustfmt`/`shfmt` produce. We must
  be honest about this in docs and never imply gofmt-parity for tier-2 languages.
- Tier-2 lints are limited to what a generic CST pass can detect; deep semantic linting
  stays tier-1 only.
- Grammar downloads add a first-run latency and a cache to manage per tier-2 language.

## Alternatives considered

- **Native backend or nothing (no tier-2):** rejected — leaves most of the xberg-io stack
  uncovered, breaking the universal promise.
- **A single full reflow/pretty-printer per grammar:** rejected for v1 — writing faithful
  formatters for 300+ grammars is intractable; that work is the ongoing tier-1 roadmap.
- **Plugin backends loaded at runtime:** rejected — see ADR 0002; in-process static
  registry keeps it pure Rust and dependency-free.
