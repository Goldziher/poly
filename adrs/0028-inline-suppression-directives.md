# 0028 — Inline Suppression Directives

- Status: Accepted
- Date: 2026-08-29
- Updated: 2026-08-29 (see "Amendment" below)

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

## Amendment — 2026-08-29 (implementation)

Two decisions taken while implementing the directive, both deviations from the letter of the
Decision above.

### 1. An unjustified directive does not suppress

The Decision says a bare `allow` with no reason "trades one finding for another", which reads as
*still suppressing* while adding a `lazy-ignore`. The implementation does **not** suppress: an
unjustified directive is inert, the original rule still fires, and `lazy-ignore` is reported
*in addition*.

The trade as written would be a genuine CI bypass. `poly lint` exits non-zero only on
error-severity findings, and `lazy-ignore` is a warning. So an unjustified directive over an
`Error` would drop the failing finding and replace it with a non-failing one — turning an empty
comment into a way to make CI green, which is precisely the outcome the mandatory-reason rule
exists to prevent. Non-suppression keeps the guard rail load-bearing: writing the directive
without a reason costs you a finding instead of buying you one.

A reason counts as present when the text after the closing bracket contains at least one
alphanumeric character (trailing `*/` and `-->` are stripped first, so `/* poly: allow[X] */` is
correctly seen as reasonless).

### 2. Comment detection is a prefix heuristic, not the `uncomment` token table

The Decision says to reuse the `uncomment` engine's per-language comment-token table. The
implementation instead tests whether the text immediately before the `poly:` marker ends with one
of a small fixed set of comment openers — `//`, `#`, `--`, `;`, `/*`, `*`, `<!--`, `%`, `!`,
`dnl`, case-insensitive `rem` — with quote characters deliberately excluded so a directive-shaped
string literal never suppresses.

The reason is the hot path. This check runs in `lint_one`, inside the rayon `par_iter` over every
file in the repository. Resolving a language's token set and scanning per-language would cost work
on every file; the union of openers is a superset that needs no language lookup at all, and the
whole mechanism is gated behind a single substring test for `poly:` so a file without a directive
— nearly all of them — pays one linear scan and allocates nothing. The residual imprecision (a
comment opener appearing inside a string literal) is accepted: it can only cause an over-broad
suppression in source nobody writes by accident, and the alternative is a parse per file.

### Also settled while implementing

- The bracketed rule list is **required**; `poly: allow F401` is not recognized and suppresses
  nothing.
- `[per-file-ignores]` is applied *after* the inline pass, so a `lazy-ignore` finding remains
  silenceable by config while staying immune to the directive that produced it (and to every
  other directive in the file).
- Suppressions are rebuilt from the current file contents on each `--fix` pass, since applying a
  fix shifts the line numbers both the directive and its target sit on.
