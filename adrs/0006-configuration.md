# 0006 — Configuration: Canonical poly.toml

- Status: Accepted
- Date: 2026-06-26
- Updated: 2026-06-28 (unified under `poly.toml` for lint/fmt/hooks/commit via
  `poly-config` crate)
- Updated: 2026-07-02 (nested `poly.toml` discovery + cascade resolution added —
  see ADR 0018. Single-nearest-ancestor discovery remains the base; `--config`
  still forces a single config and now also bypasses nesting.)
- Updated: 2026-07 (v0.9.0): clean break — `polylint.toml` is no longer read; `poly.toml`
  is the only accepted config name. The local override is `poly.local.toml`.
- Updated: 2026-08-31: the title and the passages below describing YAML as an alternate,
  auto-detected input format were never implemented and are corrected in place. `CONFIG_FILE_NAMES`
  has always had exactly one entry, `poly.toml`, parsed as TOML; there is no YAML form and never
  was one shipped.

## Context

Replacing many tools with two binaries (ADR 0001) means replacing many config files
(`pyproject.toml`/`ruff.toml`, `.prettierrc`, `.eslintrc`, `taplo.toml`, `.sqlfluff`, …)
with one. As `poly` grows into an umbrella family (lint, format, git-hooks, commit-message
linting — ADR 0011), one unified config must drive all of them. We need a single config
format that is ergonomic, comment-friendly, and round-trippable so we can read, modify, and
re-emit it without destroying user intent. Some ecosystems also lean toward YAML, so
detection should be forgiving.

## Decision

- **Canonical config is `poly.toml`** (managed by the `poly-config` crate), parsed with
  standard `toml`/serde. It is the **only** accepted config name.

  > **Update (2026-07, v0.9.0):** clean break — `polylint.toml` is no longer read or
  > accepted (reversing the original back-compat decision below). The rename to `poly` is
  > complete, so a stale `polylint.toml` is silently ignored; adopters rename it to
  > `poly.toml`.
- **`poly.local.toml` deep-merges over the primary config** when it sits in the same
  directory. Scalars and arrays replace; tables merge recursively.
- **One config drives the entire `poly` umbrella** (ADR 0011): lint, format, git-hooks,
  commit-message linting. Schema sections: `[defaults]`, `[discovery]`, `[lint]`, `[fmt]`,
  `[commit]`, `[hooks]`, `[cache]`, `[tools]`, `[per-file-ignores]`. Per-engine config
  slices (`[fmt.python.ruff]`, `[lint.js.oxc]`, …) and per-tool config (`[hooks.rust.clippy]`,
  etc.) each `Engine` or hook-runner receives as its `EngineConfig` or tool-specific settings.
- `--config <path>` overrides discovery for all poly subcommands (lint, fmt, hooks,
  commit).

## Consequences

Positive:

- One file (`poly.toml`) configures lint, format, git-hooks, and commit-message linting
  across every language and hook; onboarding is "read one config".
- `poly.local.toml` enables local development overrides (e.g. stricter rules in CI, relaxed
  rules locally) without modifying the primary config.
- One config name (`poly.toml`) with no legacy alias removes the "which file wins?" ambiguity
  entirely.

Negative / risks:

- The clean break means repos still on `polylint.toml` must rename to `poly.toml`; the old
  name is ignored rather than honored.
- A unified schema must map onto each tool's (and hook runner's) native option
  vocabulary; mismatches (options one tool has and another lacks) need deliberate,
  documented handling.
- The `poly-config` crate must maintain the growing schema as new hooks and tools are
  added (ADR 0013).

## Alternatives considered

- **Reuse each tool's native config files:** rejected — defeats the "one config" goal and
  reintroduces the fragmentation we are removing.
- **YAML or JSON as canonical:** rejected — TOML is the Rust ecosystem norm, is
  comment-friendly, and `toml_edit` gives best-in-class round-tripping. A YAML convenience
  input was considered in Context but never shipped: `poly.toml`, parsed as TOML, has been the
  only accepted config name and format from the first `poly-config` implementation.
- **No config / fully hard-coded defaults:** rejected — defaults are opinionated
  (ADR 0007) but users still need an override layer.
