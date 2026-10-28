# 0034 — Test Context: Two Mechanisms, One Per-Language Predicate Each

- Status: Accepted
- Date: 2026-10-11
- Updated: 2026-10-28 (ADR 0036): the quality tier gained `uncollected-test-file`, whose subject is a
  test file's *name*. The metric rules stay test-blind; nothing is carved out of a test.

## Context

Issue #26 asked whether "is this test code?" should become a single poly-level concept that the whole
engine consults, or stay two deliberately separate mechanisms answering deliberately different
questions — "which node" versus "which file". The worry was that whichever shape a second language
copies becomes the de facto answer, and reversing it later means rewriting both rule sets. This ADR
makes the answer explicit so it is inherited on purpose rather than by accident.

poly answers the question in two places today, and a third place deliberately does not answer it at
all:

- **Syntactically, as an ast-grep predicate.** `rust-in-test-context` (in
  `crates/poly-core/src/engines/astgrep/builtin/utils.yml`) matches a node inside a `#[cfg(test)]
  mod` or a `#[test]`/`#[bench]`-style function; five Rust rules negate it to carve test code out of
  their own match. Since the 2026-09-01 amendment to ADR 0029 it is a *global* util, shared across
  the pack.
- **By path, as a rule's own `ignores:` globs.** Four rules declare them in their own YAML
  (`unwrap-used` `**/tests/**`, `placeholder-implementation` `**/*_generated.rs`, `force-cast`
  `**/RustBridge/**`, `not-null-assertion` Kotlin test source sets). `engines/astgrep/exclusions.rs`
  merges them into `PerFileIgnores` as *defaults*; a `[per-file-ignores]` entry naming the rule
  replaces them outright.
- **`[per-file-ignores]`**, the user-facing path mechanism, is the same file-level question asked by
  configuration rather than by a rule.
- **The quality tier has no test concept, on purpose.** `file-too-long`, `function-too-long`,
  `cyclomatic-complexity` and the rest measure a metric; a test function that is 200 lines long is
  still 200 lines long, and pretending otherwise would be the metric lying.

The issue's premise — that *Rust* is the only language with a syntactic predicate, so the decision is
being taken before a second one exists — was already stale when it was filed. Go had grown
`in-test-function`, a byte-identical copy in `deep-exit.yml` and `force-type-assert.yml`. The
precedent the issue warned about had already been set, by copying a *file-local* util rather than the
shared one.

## Decision

**Keep two mechanisms, and standardise the shape of the syntactic half.**

The two mechanisms answer different questions and neither can express the other:

- A syntactic predicate cannot name a path, so it cannot keep a rule out of `tests/` when a test
  helper is written in ordinary source form.
- A path glob cannot see a `#[cfg(test)] mod tests { … }` sitting inside `src/foo.rs`, which is
  exactly where a large fraction of Rust tests live — the reason `unwrap-used` needs *both* (its
  `ignores:` covers `tests/`, its predicate covers the inline module).

The syntactic half is grammar-specific and therefore must be per-language; there is no one predicate
that compiles against Rust's `attribute_item`, Go's `testing.T` parameter and (a later language's)
test-framework call. The file-level half is user-facing and must stay a glob, because a user cannot
be asked to author a tree-sitter query to exempt a path.

What unifies the syntactic half is a convention, not a shared object:

- **One predicate per language, named `<lang>-in-test-<context>`, declared in
  `builtin/utils.yml`** — the global-util namespace ADR 0029 already established for the Rust
  helpers. Every test-context predicate is then listed in one file, in one namespace, with ids that
  cannot collide.
- **This decision moves Go's two file-local copies to `go-in-test-function` in that file**, the same
  extraction the Rust helpers received, and extends the pack's anti-drift guard
  (`the_shared_test_context_helpers_are_not_copied_back_into_rule_files`, `pack.rs`) to assert a Go
  carrier does not reintroduce a local copy.
- A rule that also needs a *file-level* carve-out declares `ignores:` in its own YAML, as the four
  rules above do. The two are used together where both apply.

The quality tier stays test-blind.

## Consequences

Positive:

- A rule author has one rule to follow: node-level carve-out → the language's global util; file-level
  carve-out → `ignores:`. The choice is forced by the question being asked, not left to taste.
- All test-context predicates are discoverable in one file, and the anti-drift guard now covers both
  languages that have one, so the seam that produced the Go copies cannot silently reopen.
- No new concept, no new config key, no runner change, and no tier that has to learn about a notion
  it currently, correctly, ignores.

Negative / risks:

- Two mechanisms remain, so a rule that needs both must write both and keep them in agreement; that
  is a real cost, accepted because each answers a question the other cannot.
- "One concept" is answered as "one naming convention plus one namespace", not as one object. A
  future reader who wanted a single predicate will find this ADR rather than a precedent, which is
  the outcome the issue asked for.

## Alternatives considered

- **One poly-level concept consulted by the ast-grep tier, the quality tier and `[per-file-ignores]`.**
  Rejected. It requires a lowest-common-denominator representation, and the only candidate that spans
  a glob mechanism and a tree-sitter predicate is a path glob — which is lossy precisely where the
  syntactic predicate is needed (inline `#[cfg(test)]` modules), so the unified concept would be less
  accurate than the two it replaces. It also drags the quality tier into a behaviour change with its
  own corpus burden, for a rule family (`file-too-long`, `function-too-long`, …) whose findings do not
  stop being true in a test.
- **Keep Go's two file-local copies.** Rejected. They are byte-identical and were already the
  precedent the issue warned about; moving them to the global namespace makes the convention true
  without exception, at the cost of one YAML block.
- **Give the quality tier a test-context notion anyway, for symmetry.** Rejected as symmetry for its
  own sake: it would change what the metric tier reports, needs its own measurement pass, and has no
  consumer asking for it.
