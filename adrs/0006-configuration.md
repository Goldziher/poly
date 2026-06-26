# 0006 — Configuration: Canonical polylint.toml, YAML Auto-Detected

- Status: Accepted
- Date: 2026-06-26

## Context

Replacing many tools with two binaries (ADR 0001) means replacing many config files
(`pyproject.toml`/`ruff.toml`, `.prettierrc`, `.eslintrc`, `taplo.toml`, `.sqlfluff`, …)
with one. We need a single config format that is ergonomic, comment-friendly, and
round-trippable so we can read, modify, and re-emit it without destroying user intent. Some
ecosystems also lean toward YAML, so detection should be forgiving.

## Decision

- **Canonical config is `polylint.toml`**, read and written with **comment-preserving
  `toml_edit`** so comments and formatting survive any tooling that rewrites config.
- A **YAML config is auto-detected** (parsed with `saphyr`) when present, but **TOML
  wins**: if both exist, `polylint.toml` is authoritative.
- The schema layers cleanly into per-engine config slices (`[fmt.python.ruff]`,
  `[lint.js.oxc]`, …) that each `Engine` receives as its `EngineConfig`.
- `--config <path>` overrides discovery for both binaries.

## Consequences

Positive:

- One file configures lint and format across every language; onboarding is "read one
  `polylint.toml`".
- `toml_edit` round-tripping enables future `polylint --init` / auto-fix-config tooling
  without clobbering user comments.
- YAML auto-detection eases migration for YAML-first repos without making YAML a second
  source of truth.

Negative / risks:

- Two accepted input formats means two parse paths to keep in sync; the "TOML wins" rule
  must be applied consistently and surfaced clearly to avoid confusion when both files
  exist.
- A unified schema must map onto each tool's native option vocabulary; mismatches
  (options one tool has and another lacks) need deliberate, documented handling.

## Alternatives considered

- **Reuse each tool's native config files:** rejected — defeats the "one config" goal and
  reintroduces the fragmentation we are removing.
- **YAML or JSON as canonical:** rejected — TOML is the Rust ecosystem norm, is
  comment-friendly, and `toml_edit` gives best-in-class round-tripping; YAML stays a
  convenience input only.
- **No config / fully hard-coded defaults:** rejected — defaults are opinionated
  (ADR 0007) but users still need an override layer.
