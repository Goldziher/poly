# 0027 — The Code-Quality Tier

- Status: Accepted
- Date: 2026-08-29
- Updated: 2026-08-29 (see "Amendment" below)

## Context

Measured with poly 0.21.12 on a 6-repo corpus: **681 findings, of which 639 (94%) were
spelling**; only 42 were code lint. On the polyglot repo `spikard`, 2,626 files were linted
and **2,039 were skipped with "no lint rules" across 23 languages** — oxlint found **zero**
issues across the other 172 TypeScript files it did lint. In every corpus repo measured,
roughly as many files got no lint at all as got linted.

poly formats 300+ languages but lints about a dozen. `Rust`, `Go`, `Java`, `Kotlin`, `C`,
`C++`, `C#`, `Elixir`, `Proto`, `Zig`, `Swift`, `Dart`, `Gleam`, `R`, `Nix`, `Ruby`, `Less`,
the HTML/template family, and the whole `Language::Other(_)` tail have no default lint
whatsoever. Rust and Go have a conditional escape hatch through the whole-project phase
(ADR 0014/0019), but only when the consuming repo has a literal `[hooks]` section, a root
`Cargo.toml`, clippy on `PATH`, and an unscoped run — a repo with no `[hooks]` section gets
zero Rust lint from poly today.

The only existing quality control in this repo was a bash hook,
`scripts/hooks/rust-max-lines.sh`: Rust-only, commit-time-only, with no span, no config key,
and no `poly lint` diagnostic — the gap `poly` itself is meant to close for everyone else.

## Decision

poly ships opinionated code-quality guard rails as **two mechanisms** — a native metric engine
and built-in ast-grep rule packs — at **warning severity, on by default**, and under a rule
that poly **never duplicates a tier-1 backend**.

1. **Metric rules go native**, in a cross-cutting `quality` engine: file/function/type length,
   nesting depth, cyclomatic complexity, parameter count, and `lazy-ignore` (a suppression
   comment with no adjacent justification). They exist to give the languages with no tier-1
   backend a uniform floor — the same threshold applied identically to Rust, Go, Java, Kotlin,
   C#, Ruby, Swift, Elixir, and the rest, which no single wrapped tool does today.
2. **Pattern rules ship as built-in ast-grep packs**, layered beneath user `[rules] dirs` and
   reusing the existing `AstGrepEngine`, its rule loader, and the `poly rules test`
   valid/invalid harness. This needs no new engine.
3. **Never re-implement what a tier-1 backend already covers.** An explicit per-language
   deferral table names each metric this tier would otherwise duplicate, commented with the
   tier-1 rule id it yields to (ruff's `C901`/`PLR0913`, oxlint's `max-depth`/`max-params`),
   and a test asserts the table stays accurate. A doubled finding — the same defect reported
   twice, by two engines, in two vocabularies — is the most likely way this tier makes poly
   worse, not better. Where a tier-1 rule exists but poly simply does not *select* it by
   default, the fix is a rule-selection change in that engine's own constant, not a new rule
   here.
4. **Warning severity, on by default** — the same posture poly already takes with `typos`.
   `poly lint` exits non-zero only on `Error`-severity findings, so a repository adopting these
   guard rails sees them immediately without a red CI.

Two further constraints on how rules are sourced and shipped:

- **Provenance: clean implementations, upstream as catalogue only.** Every rule is authored as
  poly's own ast-grep YAML from the rule *concept*; upstream linters (clippy, staticcheck, PMD,
  detekt, RuboCop, SwiftLint, Credo) are consulted only as a catalogue of *what* is worth
  detecting, never as a source of *how*. A concept ("an empty catch block is a defect") is not
  protectable; an implementation is, and writing poly's own pattern for a known concept is a
  clean implementation by construction. This matters concretely: `eslint-plugin-sonarjs` >=
  3.0 declares `LGPL-3.0-only` in its npm metadata but actually ships under the Sonar
  Source-Available License, which forbids porting to other languages — and `cargo deny` reads
  the SPDX identifier, so the license gate alone would not catch a port. The clean-
  implementation policy makes this a non-issue by construction: read a concept's description,
  not its source, and cite the origin in the rule's `note`/`url` field for the reader's
  benefit, never as a derivation claim.
- **Evidence discipline over intuition.** Rules are justified by measurement. Error and
  exception handling is the single best-evidenced target: broad exception handling is the
  top-ranked code smell across ~304k AI-attributed commits, and is independently
  over-represented as CWE-248 (uncaught exception) and CWE-390 (error condition detected
  without action) in a separate matched-control study. Several plausible-sounding candidates
  were **cut** for lack of evidence, or evidence against: single-implementation interfaces and
  static-only classes (AI-generated code measurably has *fewer* functions per file — 6.62 vs.
  11.67 — the opposite signal), "reimplements stdlib" (inverted; LLMs over-reach for
  *third-party* libraries, not the standard library), inconsistent naming (the entire
  AI-detection literature rests on AI code being *more* uniform, not less), and over-defensive
  null guards (not statically decidable without types and dataflow, which this tier does not
  have). Broad-catch rules are further scoped to dynamic languages only: a Java-specific corpus
  study puts the pattern at 0.02-0.08% there, because checked exceptions structurally suppress
  it.
- **The shipping gate.** No rule ships on by default until it has been run against the corpus
  and a random hand-read sample of its findings classified: under 10% false positives, no
  single repository accounting for more than half of all findings, and every finding
  actionable (a threshold and the observed value, not a bare assertion of complexity). Anything
  failing this gate ships opt-in, with its measured false-positive rate documented rather than
  quietly dropped or quietly forced on.

## Consequences

Positive:

- Closes the coverage gap the measurement opened: languages that get zero lint from any
  wrapped tool today get a real, spanned, configurable diagnostic.
- `scripts/hooks/rust-max-lines.sh` and its `[hooks.pre-commit.scripts.rust-max-lines]` entry
  are retired once `file-too-long` covers the same check with a span and a config key, folding
  a bespoke bash hook into the same mechanism every other language now shares.
- The deferral table and the warning-only posture bound the downside: a repository already
  well-served by ruff or oxlint sees no new noise from languages those tools already cover.

Negative / risks:

- **Hot-path risk, named explicitly.** `AstGrepEngine::lint` currently builds a fresh
  tree-sitter parse per file, unlike the tier-2 formatter, which pools parsers through a
  thread-local. Today that costs nothing because astgrep only runs where a repository has
  authored user rules, which in practice is almost never. A default-on built-in pack inverts
  that: astgrep now runs on nearly every file in every repository. It must be pooled and
  measured against the corpus before shipping on by default — this is the single largest
  hot-path risk this ADR introduces.
- A quality rule that is subtly wrong is silent and indistinguishable from a clean codebase;
  the shipping gate's "confirm it is actually catching things" half exists specifically to
  catch that failure mode, not just the noisy one.
- The deferral table is now a second thing (alongside each engine's own rule-selection
  constant) that must be kept in sync whenever a tier-1 backend's default rule set changes.

## Alternatives considered

- **Port upstream linter implementations directly** (translate a clippy/PMD/detekt check into
  poly's engine): rejected on both provenance and licensing grounds — see the
  `eslint-plugin-sonarjs` case above — and because the concept, not the code, is what
  transfers cleanly across languages poly's own rules must support that those tools never did.
- **Ship plausible-sounding rules on intuition** (stateless-class detection,
  backwards-compat-cruft, "reimplements stdlib", inconsistent-naming, defensive null guards):
  rejected — see Evidence discipline above; each either has no supporting study or is
  contradicted by one.
- **Re-implement a metric a tier-1 backend already selects** (e.g. a poly-native cyclomatic
  complexity check for Python, duplicating ruff's `C901`): rejected — this doubles findings for
  no new coverage. Where the gap is that poly does not *select* an existing tier-1 rule, the
  fix belongs in that engine's defaults, not in this tier.

## Amendment — 2026-08-29 (implementation)

### Coverage is claimed only where the tier has a structural model

`quality` is cross-cutting (`languages() == &[]`), so its first implementation answered
`provides_language_lint` with `settings.enabled` — `true` for every language. That is an
overclaim, and it silently changed user-visible output: `provides_language_lint` drives the
`no lint rules for <language>` skip, the `checked` count, the JSON `skipped` payload, and
`--deny-skips`. Adding a metric engine would have deleted the "no lint rules" skip for **every**
language in poly as a side effect.

For a language with no structural model the tier contributes only `file-too-long` (a line count)
and `lazy-ignore` (a marker scan) — exactly what a plain text file gets. Counting such a file as
linted asserts knowledge of the language that poly does not have. This repeats a principle
already established for the ast-grep backend, whose `provides_language_lint` deliberately answers
via the same per-language rule lookup `lint` performs so that a single TypeScript rule cannot
claim coverage of every language in the repository.

**The tier therefore claims a language only when it can model that language's control flow**,
which is the conjunction of two independently-probed tables: a definition query
(`definitions::has_query`) **and** a construct table (`kinds::has_table`). Those are not the same
set. A grammar with only a definition query can measure how long a function is and how many
parameters it takes, while every branch, loop and `switch` inside it is invisible, because
`nesting-too-deep` and `cyclomatic-complexity` do not run at all.

Modelled today: **Python, Rust, Go, JavaScript, TypeScript, TSX, Java, Kotlin, C, C++, C#,
Ruby.** Query-only, and therefore *not* claimed: **Zig, Swift, Dart, Gleam, Elixir, PHP, Nix,
Scala, Lua, R** — these still *report* the line-counting floor, in the same way a `typos` finding
on an unlinted language has never constituted coverage.

The predicate also subtracts the per-language deferral table: a language whose every live
structural rule is deferred to an existing backend, or switched off by user config, claims
nothing. No language reaches that state today, but it is computed rather than assumed so the
answer changes with the table instead of silently overclaiming.

Both the covered set and the deferrals are derived from the same data the lint path uses, not a
parallel hand-maintained list — two lists that must agree is precisely how this defect returns.

### Advisory severity is now enforced across backends, not just ruff

ADR 0027's "warning severity, on by default" posture was not in fact uniform: mago passed
`Level::Error` straight through, so PHP failed CI on six maintainability *metrics*
(`cyclomatic-complexity` > 15, `excessive-parameter-list` > 5, `too-many-methods` > 10,
`too-many-properties` > 10, `too-many-enum-cases` > 20, `kan-defect`). Those now report warning,
mirroring ruff's existing advisory mapping, while mago's correctness, safety and security rules
keep error severity. Measured effect: 416 findings moved from error to warning, no other severity
changed.
