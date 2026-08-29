# 0016 — Uniform Per-Tool Rule-Selection Model

- Status: Accepted
- Date: 2026-07-01
- Updated: 2026-08-29 (see "Amendment" below)

## Context

poly wraps ~15 diverse linting backends — ruff, oxc, sqruff, mago, and others — each with
its own native rule-selection vocabulary. ruff uses `select` / `ignore`, sqruff uses
`rules` / `exclude_rules`, mago uses `only`, and so on. Users migrating configurations between
tools or adopting poly's unified config face friction: each tool's idiom differs, making
it hard to reason about a consistent rule policy across the entire repository.

The goal is a single, canonical vocabulary that every backend respects, with a migration path
from native configs.

## Decision

- **One rule-selection vocabulary across all backends:** Every linter backend in poly
  accepts `select` (replace the default rule set), `extend_select` (add to the defaults),
  and `ignore` (remove from the active set) in its `[lint.<lang>.<tool>]` section. Rule
  identifiers can be code strings (e.g. `"F401"`, `"too-many-methods"`) or category names
  (e.g. `"correctness"`, `"style"`).
- **Per-rule overrides:** A `[rules.<id>]` sub-table (nested under the tool's config) allows
  rule-specific customization: a `level` key (string; one of `"error"`, `"warning"` / `"warn"`,
  `"info"` / `"information"`, `"hint"` / `"help"`) overrides the rule's severity, and any
  other key is passed as a tool-specific parameter (e.g. `[rules.cyclomatic-complexity] level =
  "warning", threshold = 10`).
- **Shared parser, mapped per-engine:** The module `crates/poly-core/src/engines/rule_config.rs`
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

## Amendment — 2026-08-29 (conformance audit)

An audit of every backend against this ADR found the model was **specified but only partly
implemented**, and that two of its promises need narrowing to stay truthful.

### 1. `[rules.<id>]` tool parameters were honoured by no backend at all

The Decision above says a `[rules.<id>]` sub-table takes "a `level` key … and any other key is
passed as a tool-specific parameter", with `threshold = 10` as the worked example. `level` was
implemented — uniformly, in the runner's post-lint `SeverityRemap`, so it works even for engines
that read no selection state of their own. **The parameter half was read by zero backends.** A
user writing `[rules.max-params] max = 6` got a key that parsed, raised no error, and did
nothing. Documentation, this ADR, and `poly migrate`'s own markdownlint importer all emitted
per-rule parameters that were silently discarded.

Now honoured by **oxlint**, **rumdl**, and **sqruff**. Still not honoured by **ruff** and
**mago**, for reasons that are dependency-shaped rather than fundamental: ruff's option types
(`mccabe::Settings`, `pylint::Settings`) are not `Deserialize` and `ruff_workspace` is not a
dependency, so it would need a hand-written code→field map; mago's `RuleSettings` is a 1:1 fit
but requires enabling mago-linter's `serde` feature, which changes the dependency and
`cargo deny` surface. Both remain reachable through each engine's flat native keys
(`mccabe_max_complexity`, …), which do work.

### 2. `extend_select` meant the opposite of its name in three backends

rumdl, ini and dockerfile implemented `extend_select` as **replace**, so
`extend_select = ["X"]` silently disabled every other rule — the inverse of this ADR and of the
documentation. ruff, mago and dotenv implemented it correctly, so poly was inconsistent with
itself. A test asserted the inverted rumdl behaviour deliberately, putting the test in direct
conflict with the ADR.

**The ADR is the specification; the behaviour was the defect.** All three now extend, and the
test was rewritten to pin the conformant semantics rather than the bug. Users who relied on the
accidental replace semantics should switch to `select`, which has always meant replace.

### 3. New limit: parameters whose native shape is positional cannot be expressed

A `[rules.<id>]` TOML table can only ever produce **one JSON object**. Rules whose native
configuration is positional — oxlint's `TupleRuleConfig` family (`eqeqeq`, `yoda`, `curly`,
`func-names`, `object-shorthand`), which take `["error", "always", {…}]` with a bare enum in
position 1 — therefore cannot be configured through this schema at all. This is a ceiling of the
uniform model itself, not of any implementation, and it narrows the "any other key" promise:
**arbitrary keys are forwarded, but only into a single options object.** Users needing a
positional configuration must fall back to the backend's own native key where one exists.

### 4. Root cause, and the change that would prevent a recurrence

Every defect above survived because `[lint.*]` and `[fmt.*]` are raw `toml::Table` values with no
schema, no `deny_unknown_fields`, and no unknown-key warning anywhere. **Every key parses by
construction**, so a key that does nothing is indistinguishable from one that works — for the
user *and* for us. The audit found the same class in the formatter surface (four documented
`[fmt.python.ruff]` keys dead, four engines' layout overrides clobbered by globals) and in dead
keys elsewhere (`[discovery] force_exclude`, several `[hooks]` keys).

Warning on unrecognised keys under `[lint.*]` / `[fmt.*]` is therefore the highest-leverage
follow-up this ADR implies. It requires each engine to declare its known option keys — or a
generated schema to check against — and is recorded here as the intended direction rather than a
decision already taken.
