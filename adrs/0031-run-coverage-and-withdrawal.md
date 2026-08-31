# 0031 — Run Coverage and Withdrawal

- Status: Accepted
- Date: 2026-08-31

## Context

`--deny-skips` / `--max-skips` exist to catch a run that quietly stopped covering part of a tree.
Two features add legitimate ways for a run to check less than a full corpus, and both landed after
`--deny-skips` did:

- **`--only <engine>` / `--skip <engine>`** restrict a run to named engines for one invocation.
  `--only ruff,typos` over a Rust file leaves every Rust-covering engine out of the plan, and the
  file reports `no lint rules for Rust` — true, and indistinguishable from the reason a
  never-configured repository gets the same message.
- **A universal `enabled` key**, accepted by every engine table whether or not the backend itself
  reads it (`[lint.python.ruff] enabled = false`), so any engine can be switched off without
  learning a backend-specific escape hatch.

Naively, a file left uncovered by either is just another skip, and `--deny-skips` would fail the
run. That makes the flag actively hostile to the feature it sits next to: a CI job running
`poly lint --only ruff --deny-skips .` to check one engine cheaply would fail on every file no
other engine reaches, for doing exactly what it was told. The same is true of a repository that
deliberately wrote `enabled = false` and then gated its own CI on `--deny-skips` — the gate would
fail on the reader's own config line.

## Decision

**A skip reason is charged to the coverage budget when it names a limit of poly, and never charged
when it names an instruction the caller gave.** Both kinds are always reported — they enter
`skipped`/`summary.skipped` identically — the split only decides whether `--deny-skips` /
`--max-skips` count them.

Charged (poly does not know what to do with this file):

- `no matching engine for this file type` — nothing claims the file at all.
- `no lint rules for <language>` — something routed the file, but the run holds no lint rules for
  its language.
- `machine-generated file ([discovery] generated = false)` — ADR 0017's generated-file skip.
- `binary file` — poly never opened it as text.

Not charged (the run is doing exactly what an instruction said):

- `no engine selected by --only/--skip for this file` (`FILTERED_SKIP`) — `--only`/`--skip` removed
  every engine covering the file for this invocation.
- `every engine disabled by config: [lint.<lang>.<engine>]` (`DISABLED_SKIP_PREFIX`) — `enabled =
  false` removed every engine covering the language in `poly.toml`.

`is_withdrawal_reason` (`crates/poly-core/src/runner/skips.rs`) is the single predicate both the
CLI's `report_skip_budget` and any other budget-consuming caller test; a skip reason is either a
plain [`SkippedFile`] the four charged reasons produce, or a [`Withdrawal`] the two uncharged ones
produce, and only `Withdrawal` reasons satisfy the predicate.

**Why the split is drawn there: the axis is who decides the *extent*, not who wrote the line.**
Every one of the six reasons traces back to something in `poly.toml` or on the command line, so
"the caller asked for it" does not separate them — `generated = false` is every bit as explicit as
`enabled = false`, and both name themselves in the reason. What separates them is whether the
caller can compute what the instruction costs them. `--only`, `--skip` and `enabled = false` name a
*tool*, and the set of files that loses coverage follows from that name by construction: whoever
wrote `--only ruff` already knows the Go files went unchecked. `generated = false` names a *key*
whose reach is decided by poly's generated-file detector — flipping it can withdraw nine files or
nine hundred, and which ones is a heuristic's answer, not the reader's. `--deny-skips` exists to
catch coverage lost without anyone noticing, and a withdrawal whose size the caller cannot work out
in advance is precisely that. The same test puts `no matching engine`, `no lint rules for
<language>` and `binary file` on the charged side: in each, the extent is poly's to decide.

Charging the two tool-scoped reasons anyway would have a predictable effect and no benefit: it
would teach people to stop using `--only`, or to stop writing `enabled = false`, in order to keep
their own gate green.

`self_manages_enabled` (`crates/poly-core/src/engine.rs`) is the companion mechanism at the engine
level: a backend that is its language's only registry slot and hands the work to the tier-2
reindenter when switched off (`native_tool`) answers `true` and takes on the whole
`enabled = false` contract itself, so the runner's blanket "drop this engine before the file loop"
handling — the thing that would otherwise produce the `DISABLED_SKIP_PREFIX` skip — never applies
to it in the first place.

## Consequences

Positive:

- `--only`/`--skip` and `--deny-skips` compose: a narrow, single-engine CI job can gate on coverage
  without being told it failed to run engines it was explicitly asked not to run.
- A repository can write `enabled = false` in its own `poly.toml` without that line failing its own
  `--deny-skips` gate — the budget only ever objects to coverage the config did not say it was
  giving up.
- The charged/uncharged split reduces to one predicate (`is_withdrawal_reason`) tested once, at the
  point `--deny-skips`/`--max-skips` filters the skip list, rather than threaded through every
  reason-producing call site.

Negative / risks:

- Two skip reasons now read almost identically in prose ("no lint rules for Rust" vs. "no engine
  selected by --only/--skip for this file") while behaving oppositely under `--deny-skips`; a
  reader must read the reason text, not just see "skipped", to know which applies. The reason
  strings themselves are the only place this is disambiguated — there is no separate `charged: bool`
  field in the JSON/TOON payload today.
- A config-driven skip is charged or not depending on which of the two axes (a file, or an engine)
  it describes, which is not visible from "was this line written by the caller" alone — see the
  cross-reference amendment in ADR 0017, added alongside this decision, for the case that reads as
  a contradiction until the axis is named.

## Alternatives considered

- **Charge every skip, regardless of cause:** rejected — makes `--deny-skips` unusable together with
  `--only`/`--skip` or a deliberate `enabled = false`, which is the scenario described in Context.
- **Charge nothing produced by a config key or flag, including `generated = false`:** rejected —
  it draws the line at explicitness, and every reason here is explicit. `[discovery] generated =
  false` hands the extent to a detector, so one flipped key can quietly stop covering hundreds of
  files that nobody enumerated; that is the silent-coverage-loss case `--deny-skips` exists for.
- **A `charged: bool` field on `SkippedFile` instead of a reason-string predicate:** rejected for
  this decision — the reason string is already unique per cause (`is_withdrawal_reason` matches by
  exact string or prefix) and adding a field is a payload-shape change with no behavioral upside;
  revisit if a consumer needs to filter charged/uncharged without string-matching the reason.
