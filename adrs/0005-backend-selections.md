# 0005 — Native Backend (Tier-1) Selections

- Status: Accepted
- Date: 2026-06-26

## Context

Tier-1 (ADR 0004) needs a specific crate chosen per language. The choice is constrained by
the dependency policy (ADR 0003, wrap-first/vendor-if-forced) and the no-subprocess rule
(ADR 0002). We lock the selections here; each is subject to the empirical `/tmp` API check
before wrapping, and may be wrapped or vendored per policy.

## Decision

v1 / launch tier-1 backends:

| Language(s) | Backend | Notes |
|---|---|---|
| JS/TS/JSX/TSX/JSON/YAML/CSS | **oxc** | `oxc_linter` + `oxc_formatter` (wrapped via pinned git dep, ADR 0003). |
| Python | **ruff** | Wrapped via pinned git dep (ADR 0003); all ruff crates share the same `rev`. |
| TOML | **taplo** | Published on crates.io. |
| Markdown | **rumdl** | Published as `rumdl_lib` on crates.io. |
| SQL | **sqruff** | Published as `sqruff_lib` on crates.io. |

Fast-follow tier-1 backends (each upgrades a language out of tier-2):

| Language(s)                  | Backend          | Notes |
|------------------------------|------------------|-------|
| CSS / SCSS / Less            | **malva**        | Published on crates.io. |
| HTML / Vue / Svelte          | **markup_fmt**   | Published on crates.io. |
| GraphQL                      | **graphql** formatter | Published on crates.io. |
| Nix                          | **alejandra**    | Pure-Rust formatter; nixpkgs-fmt rejected due to unmaintained advisories (RUSTSEC-2021-0139 / RUSTSEC-2024-0375). |
| Cross-language spell-check   | **typos**        | Published on crates.io. |
| PHP                          | **mago**         | Published on crates.io. Provides linting and formatting. |
| Ruby                         | **rubyfmt**      | Pure-Rust formatter. |
| R                            | **air** / **jarl** | Static analysis (air) and formatter (jarl). |
| HCL / Terraform              | **hcl-rs** / **hcl-edit** | Wraps hcl-rs for parsing and hcl-edit for comment-safe formatting. |
| Dockerfile                   | **markup_fmt**   | HTML/Vue/Svelte (reused via language registry). |
| XML                          | **markup_fmt**   | HTML/Vue/Svelte (reused via language registry). |

Everything not listed here is served by the tier-2 tree-sitter generic formatter.

## Consequences

Positive:

- Best-in-class, well-known tools back the highest-traffic languages, maximizing fidelity
  and user trust.
- A clear launch vs. fast-follow split keeps v1 shippable while the fast-follow set has a
  defined home.
- Each selection is a thin `Engine` adapter, so swapping a backend later is localized.

Negative / risks:

- **ruff and oxc** are pinned git deps without stable published crates; tracking upstream
  is a standing maintenance cost.
- Multiple heavy parsers (oxc, ruff, sqruff, mago) in one binary increase build time and
  size; distribution is ~70 MB per platform.
- New tier-1 backends require empirical API checks before wrapping (see ADR 0003).

## Alternatives considered

- **prettier/eslint via Node:** rejected — violates ADR 0002.
- **dprint plugins:** rejected — WASM plugin runtime; we want in-process Rust crates and
  full control over defaults.
- **Hand-rolled formatters per language:** rejected for v1 — reinventing ruff/oxc is not
  justified; that effort is better spent porting tier-2 languages to tier-1 over time.
