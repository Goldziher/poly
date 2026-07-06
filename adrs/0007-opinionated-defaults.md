# 0007 — Opinionated Defaults: Tool Defaults Plus a Thin Override Layer

- Status: Accepted
- Date: 2026-06-26

## Context

A universal tool can either expose every knob of every backend (overwhelming) or impose a
single rigid style (alienating). We want strong, consistent defaults that work out of the
box, while still respecting the considerable design effort already baked into each wrapped
tool's own defaults. We explicitly do **not** replicate the xberg-io configs — that
research is inspiration only.

## Decision

Configuration resolves in three layers: **tool default → opinionated override → user
`poly.toml`** (ADR 0006).

- **Base = each wrapped tool's own defaults.** We do not re-derive style from scratch; we
  inherit ruff's, oxc's, taplo's, etc. defaults as the foundation.
- **Thin opinionated override layer** on top, deliberately minimal:
  - **Line length 120** everywhere a tool exposes the setting.
  - **Always format docstrings** (`docstring_code_format = true`,
    `docstring_code_line_length = 120`).
  - **Purely stylistic rules:** pick one modern, consistent convention or turn the rule
    off — never bikeshed.
  - Sensible whitespace hygiene defaults (LF line endings, final newline).
- **User `poly.toml` always wins** over both layers.
- Optionally honor `.editorconfig` for indent/line-length where present.

## Consequences

Positive:

- Works well with zero config; the override layer is small enough to reason about fully.
- Inheriting tool defaults keeps us aligned with each ecosystem's norms and reduces
  surprise for users coming from the underlying tool.
- "One convention or off" eliminates style debates and keeps diffs stable.

Negative / risks:

- Line-length 120 and other choices are opinions; some users will disagree, hence the user
  override layer must be reliable and well-documented.
- Layering across heterogeneous tools is fiddly: each backend exposes line-length,
  docstring, and stylistic toggles differently, so the override mapping is per-engine work.
- Tier-2 languages (ADR 0004) honor only what generic formatting can express (indent,
  whitespace, line endings); the docstring/line-length overrides may not apply to them.

## Alternatives considered

- **Expose all backend options 1:1:** rejected — recreates per-tool config sprawl and the
  cognitive load we set out to remove.
- **Single immutable house style, no overrides:** rejected — too rigid for real repos with
  legacy constraints.
- **Replicate the xberg-io configs verbatim:** rejected — those are inspiration only;
  we want defaults derived from tool defaults plus a principled thin layer.
