# 0028 — Inline Suppression Directives

- Status: Accepted
- Date: 2026-08-29

## Context

ADR 0017 gave poly one suppression mechanism: `[per-file-ignores]`, a top-level config table
mapping gitignore-style path globs to rule codes. That is deliberately whole-file (or
whole-glob) scoped, and it works well for the case it targets — a generated file or a test
directory that should never see a given rule. It has no answer for a single legitimate
exception inside an otherwise normal file: with only a file-level glob available, the sole
remedy for one justified case is disabling the rule for the *whole file*, or the whole
repository if no narrower glob exists. ADR 0027's on-by-default quality rules make this gap
immediate rather than theoretical — a repository adopting them will hit a real exception on
day one and needs a way to say so without weakening the rule everywhere else.

It is also required for internal coherence. ADR 0027's `lazy-ignore` rule demands a
justification on every `#[allow(…)]` / `# noqa` / `// eslint-disable` it finds; poly cannot
credibly enforce that standard on other tools' suppressions while offering only an unjustified,
glob-based escape hatch for its own.

## Decision

poly gains a `// poly: allow[rule-id] reason` inline directive, using each language's own
comment token, plus a file-level `// poly: allow-file[rule-id] reason` form. **A reason is
mandatory in both forms**; a bare `allow` with no reason is itself a `lazy-ignore` finding.
This is the entire point of the mechanism as a guard rail rather than a bypass: an agent (or a
developer in a hurry) cannot silence a rule by writing an empty comment, because doing so
trades one finding for another.

This amends ADR 0017, which established `[per-file-ignores]` globs as poly's only suppression
mechanism; that ADR's "Updated" note points here.

- **Applied centrally in the runner**, alongside the existing `PerFileIgnores` filter, so the
  directive works uniformly for every engine — ruff, oxlint, and the new quality/ast-grep tiers
  alike — rather than being wired per-backend.
- **Reuses the `uncomment` engine's existing per-language comment-token table** rather than
  building a second one. poly already knows every language's comment syntax for the purpose of
  stripping comments; suppression directives are a second consumer of the same knowledge, not
  a reason to duplicate it.
- **Every language's directive ships with a test proving it actually suppresses**, run against
  the real pipeline rather than only against a unit test of the parser. thai-lint's
  `# thailint: ignore[file-placement]` is documented but read by no code at all — a suppression
  directive that looks wired but is not is worse than no suppression mechanism, because it
  gives a false sense of an escape hatch that never fires.

## Consequences

Positive:

- Pairs directly with ADR 0027's on-by-default guard rails: a single legitimate exception no
  longer forces disabling a rule file-wide or repo-wide.
- The mandatory-reason requirement keeps the mechanism from becoming a silent bypass, and keeps
  poly internally consistent with what its own `lazy-ignore` rule demands of other tools'
  suppressions.
- One implementation in the runner covers every engine, so new backends inherit inline
  suppression for free rather than needing their own comment-syntax integration.

Negative / risks:

- poly now has two suppression mechanisms — `[per-file-ignores]` (file-scoped, config-file
  based) and the inline directive (line-scoped, in-source) — and users must learn both and know
  which applies where. Documentation must be explicit that the inline form is for a single
  exception and the config form is for a whole file or class of files.
- An inline directive that is documented but silently unwired for one language would be a
  worse outcome than the file-level-only status quo, since it would look like a working escape
  hatch. The per-language suppression test is the direct mitigation, not a formality.

## Alternatives considered

- **Rely on `[per-file-ignores]` alone, with no inline mechanism:** rejected — it is
  structurally too coarse for a single justified exception in an otherwise-clean file, and
  ADR 0027 makes that gap immediate rather than theoretical.
- **Allow a bare `allow` with no reason, matching most tools' native suppression comments:**
  rejected — it would let a rule be silenced with an empty comment, undermining the guard-rail
  purpose of the mechanism and directly contradicting poly's own `lazy-ignore` rule.
- **Per-tool native suppression syntax** (ruff's `noqa`, oxc's `// eslint-disable`, sqruff's
  `-- noqa`) **instead of a poly-owned directive:** rejected for the same reason ADR 0017
  rejected it for file-level suppression — divergent syntax across ~15 backends the user would
  have to learn, and the new quality and ast-grep-pack tiers have no native suppression syntax
  of their own to reuse in the first place.
