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
| JS/TS/JSX/TSX/JSON/YAML/CSS | **oxc** | `oxc_linter` + `oxc_formatter`; vendor oxfmt if not exposed. |
| Python | **ruff** | No stable lib API; vendoring likely (ADR 0003). |
| TOML | **taplo** | Wraps cleanly (expected). |
| Markdown | **rumdl** | `rumdl_lib`. |
| SQL | **sqruff** | `sqruff_lib`. |

Fast-follow tier-1 backends (each upgrades a language out of tier-2):

| Language(s)                  | Backend          |
|------------------------------|------------------|
| CSS / SCSS / Less            | **malva**        |
| HTML / Vue / Svelte          | **markup_fmt**   |
| GraphQL                      | **graphql** formatter |
| Nix                          | **nixpkgs-fmt**  |
| Cross-language spell-check   | **typos**        |

Everything not listed here is served by the tier-2 tree-sitter generic formatter.

## Consequences

Positive:

- Best-in-class, well-known tools back the highest-traffic languages, maximizing fidelity
  and user trust.
- A clear launch vs. fast-follow split keeps v1 shippable while the fast-follow set has a
  defined home.
- Each selection is a thin `Engine` adapter, so swapping a backend later is localized.

Negative / risks:

- **ruff** is the highest-risk backend: vendoring a large, fast-moving codebase with no
  API stability guarantee is a standing maintenance cost.
- **oxc**'s formatter API maturity is uncertain; we may have to vendor oxfmt and track its
  evolution.
- Multiple heavy parsers (oxc, ruff, sqruff) in one binary increase build time and size.
- YAML comment-preservation strategy (oxc fmt vs a yaml-edit approach) is unresolved and
  decided during implementation.

## Alternatives considered

- **prettier/eslint via Node:** rejected — violates ADR 0002.
- **dprint plugins:** rejected — WASM plugin runtime; we want in-process Rust crates and
  full control over defaults.
- **Hand-rolled formatters per language:** rejected for v1 — reinventing ruff/oxc is not
  justified; that effort is better spent porting tier-2 languages to tier-1 over time.
