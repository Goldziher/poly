# 0032 — The Hardening Corpora

- Status: Accepted
- Date: 2026-08-31

## Context

poly's own test fixtures are small, hand-written, and chosen to exercise a known path. They cannot
answer the questions that decide whether a release is safe to ship, or whether a lint rule can be
on by default: does poly error on anything in a large tree nobody wrote for us; does formatting
converge, or do two backends fight over the same file; does the cache ever serve a wrong answer;
how many findings does a rule actually produce on code we did not write. ADR 0027's amendment and
ADR 0029's default-on audit both needed exactly that last question answered against real code, and
answered it ad hoc each time — a repeatable harness, run the same way in CI, was the missing piece.

The harness (`scripts/harden.sh`) needs real code to run against, and not all real code can answer
the same question. A count taken against a tree that changes underneath the harness is not
reproducible — the same rule run twice reports different numbers for a reason that has nothing to
do with poly. An invariant does not have that problem: "no file errored" or "formatting converges
in two passes" is true or false regardless of what the tree contains.

## Decision

**An invariant can be asserted on any input. A count needs a pinned one.** Three corpora, split by
what they are allowed to assert rather than by any other property:

| | A — local siblings | B — pinned OSS | C — generated code |
| --- | --- | --- | --- |
| Where | `../*`, in place | a disposable clone | a disposable clone |
| Acquisition | none | `git fetch --depth=1 <sha>` | same |
| Stability | unstable, may be dirty | pinned to a commit | pinned, population is a judgement |
| Written to | never | disposable | disposable |
| Runs in CI | no — private trees | nightly | `workflow_dispatch` |
| Invariants | gate | gate | gate |
| Per-rule counts | trend only | **gate** | audit input |

- **Corpus A — the sibling working trees next to this repo.** Live checkouts, so the harness never
  writes to them: per-file linting runs in place through `poly_core` directly (never the CLI, whose
  whole-project phase would execute `cargo` against the worktree), formatting runs against a
  disposable copy, and the result cache is redirected under `POLY_HARDEN_ROOT`. Unstable by
  construction — a sibling can be mid-edit or absent — so its per-rule counts are trend data only,
  never a gate; only the invariants gate here, and they may run outside CI since the trees are
  private.
- **Corpus B — pinned third-party trees**, fetched at `git fetch --depth=1` against an exact 40-hex
  commit sha, never a branch. This is the only corpus whose **counts** may gate a rule's default
  severity or a release, because a pinned input is the only input where a count taken today and a
  count taken from the same manifest tomorrow describe the same code. Bumping a sha is a deliberate
  commit, which is the review moment a change in the numbers gets looked at. Licences are checked
  against an allow-list before anything is cloned (nothing copyleft — a CI artifact derived from a
  tree is a redistribution question even though nothing is linked), and findings are carried only
  as `path:line` and counts — the record type has no field that can hold source text.
- **Corpus C — generated code**, today codegen output (SDKs and clients emitted by a generator).
  Pinned like B, but the *population* the manifest names is a judgement call in a way a commit sha
  is not, so its counts are audit input, not a gate. A green result over corpus C says nothing about
  LLM-authored application code, a distinct population `docs/harden-corpus.md` gives its own
  acquisition procedure (identify by agent artifacts, corroborate from commit-trailer history,
  filter, hand-verify a sample, publish the query) — deliberately left unfilled until that procedure
  is followed, rather than filled with a plausible-looking guess.

Each root runs as its own process, so a panic in one repository cannot take the rest of the run
with it, and each root appends its NDJSON record to the results file **before** its own assertions
run, so a failing root still leaves its measurement behind. Every threshold a root was judged
against is written into its own record, so a result read months later is self-describing without
cross-referencing the script version that produced it.

## Consequences

Positive:

- A rule's default severity, or a release's "did anything error" claim, now has a documented,
  reproducible input to point at, rather than an ad hoc run someone did once.
- The invariant/count split means a corpus does not need to be pinned to be useful — corpus A
  catches an outright crash or a non-convergent formatter on real, currently-evolving code, at zero
  acquisition cost, precisely because it is never asked for a count.
- Per-root NDJSON records with embedded thresholds make a past result auditable without needing the
  script version, or the corpus state, that produced it.

Negative / risks:

- Corpus B's per-rule counts, being the only gating ones, are also the only ones a maintainer must
  actively re-baseline (bump the sha) when the pinned trees' code legitimately changes shape —
  an unmaintained pin slowly drifts from representing current idiomatic code.
- Corpus B is "idiomatic, reviewed, hand-written open source", which systematically under-reports
  the patterns the AI-quality rules exist for; a rule audited only against B is audited against the
  population least likely to trip it. Corpus C exists to cover that gap but is explicitly not yet
  filled for the application-code half.
- The repositories that are themselves heavily agent-written (poly and its siblings) are
  deliberately corpus A, not corpus C — treating our own Rust-heavy, Rust-idiomatic repositories as
  "the AI corpus" would audit AI-authored code against almost no non-Rust language.

## Alternatives considered

- **One corpus, always pinned:** rejected — it forecloses corpus A's whole value, which is
  exercising the harness against trees nobody curated for this purpose at zero acquisition cost.
  The invariants corpus A gates do not need reproducibility to be worth running.
- **One corpus, never pinned:** rejected — no count taken against a moving input survives being
  gated; the first time somebody else's ordinary commit shifts a finding count, the check either
  false-fails or gets disabled, and the invariants would be lost along with it since they share the
  same corpus.
- **Gate counts from corpus C too:** rejected — the manifest that defines corpus C's population is
  itself a judgement call (which repositories count as "agent-written"), unlike a commit sha, so a
  count against it answers "how did this rule behave against the repositories we picked", not "how
  will it behave in general". Treated as audit input instead.
