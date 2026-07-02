# 0006 — Configuration: Canonical poly.toml, YAML Auto-Detected

- Status: Accepted
- Date: 2026-06-26
- Updated: 2026-06-28 (unified under `poly.toml` for lint/fmt/hooks/commit via
  `poly-config` crate)
- Updated: 2026-07-02 (nested `poly.toml` discovery + cascade resolution added —
  see ADR 0018. Single-nearest-ancestor discovery remains the base; `--config`
  still forces a single config and now also bypasses nesting.)

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
  standard `toml`/serde.
- **`polylint.toml` is still accepted** for backward compatibility, but `poly.toml` takes
  precedence if both exist.
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
- Backward compatibility: repos with `polylint.toml` continue to work without changes.

Negative / risks:

- Three accepted input formats (`poly.toml` > `polylint.toml` > YAML) means three parse
  paths to keep in sync; the precedence rule must be applied consistently and surfaced
  clearly to avoid confusion when multiple files exist.
- A unified schema must map onto each tool's (and hook runner's) native option
  vocabulary; mismatches (options one tool has and another lacks) need deliberate,
  documented handling.
- The `poly-config` crate must maintain the growing schema as new hooks and tools are
  added (ADR 0013).

## Alternatives considered

- **Reuse each tool's native config files:** rejected — defeats the "one config" goal and
  reintroduces the fragmentation we are removing.
- **YAML or JSON as canonical:** rejected — TOML is the Rust ecosystem norm, is
  comment-friendly, and `toml_edit` gives best-in-class round-tripping; YAML stays a
  convenience input only.
- **No config / fully hard-coded defaults:** rejected — defaults are opinionated
  (ADR 0007) but users still need an override layer.
