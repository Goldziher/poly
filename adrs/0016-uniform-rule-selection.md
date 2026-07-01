# 0016 — Uniform Per-Tool Rule-Selection Model

- Status: Accepted
- Date: 2026-07-01

## Context

polylint wraps ~15 diverse linting backends — ruff, oxc, sqruff, mago, and others — each with
its own native rule-selection vocabulary. ruff uses `select` / `ignore`, sqruff uses
`rules` / `exclude_rules`, mago uses `only`, and so on. Users migrating configurations between
tools or adopting polylint's unified config face friction: each tool's idiom differs, making
it hard to reason about a consistent rule policy across the entire repository.

The goal is a single, canonical vocabulary that every backend respects, with a migration path
from native configs.

## Decision

- **One rule-selection vocabulary across all backends:** Every linter backend in polylint
  accepts `select` (replace the default rule set), `extend_select` (add to the defaults),
  and `ignore` (remove from the active set) in its `[lint.<lang>.<tool>]` section. Rule
  identifiers can be code strings (e.g. `"F401"`, `"too-many-methods"`) or category names
  (e.g. `"correctness"`, `"style"`).
- **Per-rule overrides:** A `[rules.<id>]` sub-table (nested under the tool's config) allows
  rule-specific customization: a `level` key (string; one of `"error"`, `"warning"` / `"warn"`,
  `"info"` / `"information"`, `"hint"` / `"help"`) overrides the rule's severity, and any
  other key is passed as a tool-specific parameter (e.g. `[rules.cyclomatic-complexity] level =
  "warning", threshold = 10`).
- **Shared parser, mapped per-engine:** The module `crates/polylint-core/src/engines/rule_config.rs`
  provides `RuleSelection::from_options()` to parse the uniform schema, yielding a `RuleSelection`
  struct with `select`, `extend_select`, `ignore`, and `rules` fields. Each backend then maps
  this normalized selection onto its native rule mechanism — e.g. ruff's `RuleSelector`, oxc's
  `with_filter`, sqruff's `allow` / `deny` lists.
- **Back-compat aliases:** User configs that use a tool's native keys are accepted. sqruff's
  `rules` / `exclude_rules` map to `select` / `ignore`; rumdl's `enable` / `disable` map to
  `extend_select` / `ignore`. Unknown rule or category names error loudly (warn + skip the
  invalid code) rather than silently dropping them.
- **Unrecognized rule levels are safe:** If a user specifies an unrecognized `level` value,
  it is logged as a warning and the engine falls back to its own default severity for that rule.
  The rule is still applied; the override just fails gracefully.

## Consequences

Positive:

- One canonical vocabulary: users learn one set of keys and use them everywhere, reducing
  cognitive load and config migration friction.
- Engine-agnostic policy: a repository can declare a rule policy once in `poly.toml` and apply
  it uniformly across Python (ruff), JavaScript (oxc), SQL (sqruff), Python dataclass docstrings
  (mago), and beyond, without rewriting the intent for each tool.
- Reusable infrastructure: downstream tools and editors can consume the same config format,
  lowering integration burden.

Negative / risks:

- Tool-specific subtleties are hidden: some rules are engine-specific (e.g. mago's `too-many-lines`
  has no equivalent in oxc). The uniform schema cannot express every tool's native nuance; power
  users must sometimes reach for tool-specific params in the `[rules.<id>]` sub-table to get
  exact behavior.
- Category names vary by tool: ruff's `"F"` (pyflakes) does not exist in oxc. A user selecting by
  category must understand which tools expose which categories, or select by the explicit code
  names that all tools share. Guidance and documentation mitigate this.
- Migration complexity: users converting from native configs must understand the mapping (e.g.
  sqruff's `rules` → `select`), and templates or migration tooling are needed to avoid manual
  rewrites of large configs.

## Alternatives considered

- **Tool-native schemas only (no abstraction):** rejected — retains friction at the point of
  config authoring and tool integration.
- **Only per-rule overrides, no `select` / `extend_select` / `ignore`:** rejected — the ability
  to swap out the entire rule set (e.g. "enable only correctness checks for this tool") is
  central to rule policy; per-rule tweaks alone cannot express it.
- **Dynamic rule discovery (expose every tool's rules in the schema):** rejected — the schema
  would balloon with ~100+ tool-specific rule names, and it would be brittle when upstream tools
  add rules. The simple string-code approach is more stable and extensible.
