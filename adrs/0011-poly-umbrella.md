# 0011 — The poly Umbrella: One Binary Family, One Config

- Status: Accepted
- Date: 2026-06-28
- Updated: 2026-07 (v0.9.0): the umbrella became a **single `poly` binary** — the
  `polylint` / `polyfmt` back-compat entrypoints were dropped (clean break), and the shared
  cache moved out of the repo into the per-user OS cache dir (ADR 0008).

## Context

The initial mission (ADR 0001) was two binaries: `polylint` (lint) and `polyfmt` (format).
As the project matured, the value proposition expanded beyond static analysis. Two
additional tools became obvious targets for the same "replace your entire tool sprawl"
model: git-hooks (the predecessor was `.pre-commit-config.yaml` + prek) and commit-message
linting (via gitfluff). Each tool had its own config surface and entry point. The natural
next step is unifying all four under one name, one config, and one conceptual family,
reducing friction for users who already rely on polylint/polyfmt to manage their
linter/formatter stack.

## Decision

- **Introduce the `poly` umbrella command** that drives lint, format, git-hooks, and
  commit-message linting. The binaries remain `polylint` and `polyfmt` for backward
  compatibility; `poly` is the preferred modern entrypoint.

  > **Update (2026-07, v0.9.0):** clean break — `poly` is the **only** binary. The
  > `polylint` / `polyfmt` entrypoints were removed (they were only ever hook IDs, never
  > shipped binaries); the work is done via `poly lint` and `poly fmt` subcommands
  > (alongside `hooks`, `commit`, `rules`, `cache`, `mcp`, `migrate`).
- **One config (`poly.toml`) drives all four workloads** (see ADR 0006). Sections:
  `[lint]`, `[fmt]`, `[hooks]`, `[commit]`, and `[cache]`. Users write one config file
  and all four tools respect it.
- **Consume `gitfluff` as an internal library.** Commit-message linting becomes `poly
  commit` (subcommand driven by `[commit]` config); users install one binary instead of
  two.
- **Replace the prek bridge with a native hook runner (`poly hooks`).** No more
  `.pre-commit-config.yaml` → temp YAML → external `prek` subprocess. The new `poly
  hooks` engine (the `poly-hooks` crate) is in-process, rayon-driven, and driven by
  `[hooks]` config. See ADR 0012.
- ~~**Keep backward compatibility:** `polylint` and `polyfmt` remain standalone entry
  points with their original semantics. `poly` is the umbrella and the recommended future
  path.~~ **Reversed (2026-07, v0.9.0):** the standalone entrypoints were dropped; `poly` is
  the single binary and its subcommands are the only surface.

## Consequences

Positive:

- One config, four tools, unified versioning (ADR 0010). A repo's setup story simplifies
  to "read `poly.toml`" and install one `poly` binary.
- Hooks and commit-message linting are now part of the core `poly` identity, not add-ons.
- `poly-config` manages one schema shared by all four subcommands; changes to the config
  structure propagate uniformly.
- The cache (ADR 0008) is unified: all four workloads store results in the per-user OS cache
  dir (`~/.cache/poly/<repo-key>`) with appropriate namespacing.

Negative / risks:

- The `poly` CLI must be discoverable and its UX consistent across four subcommands
  (lint, fmt, hooks, commit). Documentation and help text must stay in sync.
- `gitfluff` and the prek-derived hook runner are now core dependencies; their evolution
  is now our concern.
- Adopters of the old `polylint.toml` + `polyfmt` + `.pre-commit-config.yaml` stack must
  migrate to `poly.toml`. Migration tooling is out of scope for v1 but recommended.

## Alternatives considered

- **Keep separate binaries and configs:** rejected — the value of the umbrella is
  unification; separate tools defeat that.
- **Require `poly` CLI and deprecate `polylint`/`polyfmt`:** originally rejected in favor of
  coexistence. **Reversed (2026-07, v0.9.0):** `poly` is now the sole entrypoint; the
  back-compat aliases were removed once the rename was complete.
- **Use `poly-cli` as the binary name instead of `poly`:** rejected — `poly` is shorter,
  mnemonic (short for "polyglot"), and avoids redundancy.
