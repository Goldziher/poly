# 0029 — The Built-In ast-grep Rule Pack

- Status: Accepted
- Date: 2026-08-29
- Updated: 2026-08-31: `undocumented-unsafe-block`'s `#[cfg(test)]` carve-out (commit 3ca3f34)
  meets the precondition recorded below; the rule still ships `off` (see the amendment at the
  end).

## Context

poly wraps `ast-grep` as a cross-cutting backend (`engines/astgrep/`), and until now that
backend shipped with **no rules at all**. It matched a language only when a repository pointed
`[rules] dirs` at YAML files of its own. In a repository that had never heard of ast-grep — that
is, nearly all of them — the engine parsed nothing, matched nothing, and contributed nothing.

That left a real hole, and ADR 0027 made it visible rather than creating it. The coverage
accounting introduced there answers one question per file: does anything in this run hold a rule
that knows this language? Tier-2 (ADR 0004) answers *no* — it reindents and normalizes
whitespace, which is formatting, not lint. The `quality` tier answers *yes* for the twelve
languages it structurally models. Everything else — Swift, Kotlin, Ruby, Elixir, C#, and the long
tail — got `no lint rules for <language>` and left the `checked` count.

The honest read of that output is that poly's lint coverage stopped at the languages with a
tier-1 crate backend plus the ones `quality` models. Closing the gap by writing a tier-1 backend
per language is not available: there is no Rust crate that lints Swift or Kotlin, and writing one
is a project, not a task. But ast-grep is already compiled in, already per-file, already pure
Rust, and its rule format is declarative YAML. The missing piece was never the engine. It was
that we shipped it empty.

## Decision

**poly embeds a curated rule pack in the binary, on by default.**

Twenty-six rules across nine languages (C#, Elixir, Go, Java, Kotlin, Python, Ruby, Rust, Swift),
each a YAML file under `engines/astgrep/builtin/<language>/`, embedded with `include_str!` and
compiled at first use into a `OnceLock`.

Four properties make this an addition to the existing engine rather than a second mechanism:

- **One parse path.** Pack rules go through the same `ast_grep_config::from_yaml_string` call
  user rules do. There is no pack-specific schema, no second parser, and no rule a user could not
  have written themselves. The only difference is that the YAML text is a compile-time constant
  instead of a file read — which is also why the pack is cached in a `OnceLock` rather than keyed
  by content hash: it cannot change without a new binary.
- **The pack sits beneath user rules.** `resolve_rules` merges by `id`; a user rule sharing a
  pack rule's `id` replaces it outright. The pack fills gaps in a repository's own rules and never
  overrides them.
- **Default state is authored in the rule, not in code.** Each YAML carries its own `severity:`,
  including `off` — **13 of the 26 ship off** after the audit below.
  `[lint.astgrep]` `select` / `extend_select` / `ignore` and
  `[lint.astgrep.rules.<id>] level` move any of them, exactly as for a user rule (ADR 0016).
  **Two tables named `rules` are in play and must not be conflated:** the *top-level*
  `[rules]` holds `dirs` and `builtin` (`[rules] builtin = false` disables the pack wholesale),
  while the per-rule override table is nested inside the engine's own table — ADR 0016 writes it
  as `[rules.<id>]` relative to that engine, which in full is `[lint.astgrep.rules.<id>]` here
  and `[lint.<lang>.<tool>.rules.<id>]` for a language backend
  (`crates/poly-core/src/engines/rule_config.rs`).
- **Coverage is answered by the same lookup that lints.** `provides_language_lint` calls
  `resolve_rules` and reports coverage when the resulting rule set is non-empty — so the claim is
  computed from the rules that are about to run, never from "the engine is switched on". This is
  the discipline ADR 0027's amendment records for `quality`, applied here for the same reason.

Every rule is authored from the concept. Upstream rule catalogues (ESLint plugins, SwiftLint,
detekt, …) are a source of *what is worth checking*, never of *how it is implemented*; the origin
is cited in each rule's `note` / `url`.

## Consequences

Positive:

- Nine languages gain genuine lint coverage with no configuration, on a mechanism that was
  already paid for. Swift and Kotlin go from "poly formats this and tells you it has no rules" to
  a real, if small, rule set.
- The pack is the cheapest possible place to add a language-specific check: a YAML file and its
  test fixture, no Rust, no registry change, no new engine.
- Coverage accounting stays honest in both directions. Languages the pack reaches leave the skip
  set because something really does lint them; languages it does not reach keep the skip.

Negative / risks:

- **`externally_linted_languages` (ADR 0027 / `poly-cli/src/workspace_coverage.rs`) is now inert
  under shipped defaults.** It exists to stop the per-file tier reporting `no lint rules for Rust`
  in a run whose whole-project phase ran `cargo clippy`. It maps exactly two languages — Rust via
  clippy, Go via golangci-lint / go-vet / staticcheck — and the pack plus `quality` now cover both
  per-file, so there is no skip left for it to retract. The mechanism remains correct and still
  fires for a repository that sets `[lint.quality] enabled = false` and `[rules] builtin = false`,
  which is the only way its end-to-end tests can now reach the state they describe. It is kept
  rather than removed because the condition it guards is a property of the coverage tables, and
  those change.
- **A pack rule cannot express a path exclusion.** An ast-grep rule matches AST nodes, not paths,
  and poly's general mechanism for this — `[per-file-ignores]` (ADR 0017) — is user-config only,
  with no channel for a default shipped by an engine. Three rules measured badly enough on the
  corpus to need one anyway, so the pack carries a hardcoded `NOISY_PATH_EXCLUSIONS` table scoped
  to **four rule ids** — `unwrap-used`, `placeholder-implementation` (320 of 324 findings in one
  generated file), `force-cast` (662 of 662 in `swift-bridge`'s own generated glue) and
  `not-null-assertion` (103 of 103 under Kotlin test source sets). **A user cannot opt back in for
  those ids on those paths.** This is a real limitation and a stand-in for engine-supplied
  `[per-file-ignores]` defaults, not a design.

  The rule that bounds it, adopted during the audit: **a rule may lean on this table only while a
  *minority* of its exposure is unaddressable noise.** A rule that path exclusion cannot get under
  control ships `off` instead of growing the table — which is what happened to
  `undocumented-unsafe-block`.
- Every rule the pack adds is warnings a user did not ask for. `poly lint` exits non-zero only on
  error severity, so no CI turns red — but a first run that prints thousands of warnings is its
  own kind of failure, and the volume of a default-on rule is part of whether it ships on. The
  ship-on-by-default gate is unchanged: hand-read a sample of at least fifty findings, require an
  observed false-positive rate under 10%, require no single repository to account for more than
  half the findings, and require that the rule demonstrably fires — a rule silent because its
  pattern is wrong is indistinguishable from a clean corpus.
- The pack grows the binary and the compile-time constant table. Twenty-six rules is negligible;
  a pack an order of magnitude larger would need the rules moved out of `include_str!`.

## The default-on audit (2026-08-29)

Every rule shipping a severity other than `off` was measured against the 48-root corpus, and
findings were **hand-read in full file context** rather than classified from grep lines. Three
Rust rules were flipped off as a result. They fail for one shared reason, which is worth stating
once rather than three times: **they are `restriction`-class lints whose corpus mass sits in code
the repository's authors did not write and cannot fix.** A finding that is accurate and
unactionable costs a reader the same attention as one that is wrong.

| rule | corpus | verdict |
|---|---|---|
| `allow-attribute-without-reason` | 13,622 — the largest of all 26 | **off**: 60 sampled, dominated by `#[allow(non_snake_case)]` on FFI bindings and macro-generated glue. Self-explanatory by construction, not drive-by mutes. |
| `undocumented-unsafe-block` | 6,756 → 5,475 after exclusion | **off**: classified by origin — 94.4% vendored FFI-binding code, 4.5% `unsafe { env::set_var(..) }` inside `#[cfg(test)]`, **60 findings (~1%) genuinely first-party**. |
| `unwrap-used` | 3,842 → ~1,780 | **off**: 12 read in full context, 9 safe-by-invariant or ecosystem-idiomatic (`Mutex::lock`, regex literals, `Captures::get(0)`, post-guard `.next()`, `fmt::Write`, `CARGO_MANIFEST_DIR`). |

Two findings from that pass generalise beyond these rules.

**A rule can be wrong about a context regardless of volume.** `undocumented-unsafe-block`'s 4.5%
slice is `unsafe { std::env::set_var(..) }` in test modules — under the 2024 edition that wrapper
is the *only* legal way to mutate the environment, so the rule fires on code the language forces
the author to write. That is a carve-out gap (`#[cfg(test)]`), and it would remain a defect at a
tenth the volume. Its YAML records the carve-out as a precondition for any future revival. **That
precondition is now met** (commit 3ca3f34 added the `#[cfg(test)]` exclusion); the rule still
ships `off` — meeting the carve-out precondition clears the defect this passage names, not the
94.4% vendored-FFI slice above it, which is a separate, larger reason the rule has not been
revisited. See the amendment below for the mechanism that keeps five near-identical copies of the
carve-out's test-context predicate in sync.

**Vendored third-party source carried in-tree is a distinct noise class from generated code**, and
no path glob reaches it: it sits at first-party paths under first-party names. One case was
identified only by reading an upstream copyright header in a crate labelled a "maintained fork".
The general fix belongs with `[per-file-ignores]` defaults or discovery, not in this pack.

**Silence was reported as silence.** Seven rules — `eval-usage` (Ruby), `preserve-stack-trace` and
`empty-catch-block` (Java), `async-void` and `rethrow-loses-stack` (C#), `unused-operation`
(Elixir), and `defer-in-loop` (Go) — ship on with **no corpus exposure**, recorded as *not
validated* rather than inferred clean. A rule silent because its pattern is wrong is
indistinguishable from a clean corpus, and the fixture test only proves it fires on the one case
its author wrote.

## Amendment — 2026-08-31: JS/TS and Python were audited and no rule was added

Issues #23 and #24 asked for JavaScript/TypeScript rules — the largest agent-written surface, and
the one the pack does not reach at all — and for the two `off` Python rules to earn a default. Five
candidates were drafted and measured against ten pinned repositories, four of them with LLM-authored
histories by commit trailer and two human-authored controls. `docs/pack-rule-audit.md` holds the
per-rule numbers and the hand-read false-positive rates; the corpus is the C2 section of
`scripts/harden/repos.c.tsv`, recorded as a selection *procedure* rather than a list, because a list
goes stale and a procedure can be re-run.

**None of the five shipped, and that is the finding rather than a failure to deliver.** Three fail
for one semantic reason: the construct they match is usually deliberate. `throw new
Error("Method not implemented.")` is what TypeScript's own quick fix writes and means "unsupported
here"; `raise NotImplementedError` spells "abstract" or "this backend does not do that"; `.catch(()
=> {})` is overwhelmingly best-effort cleanup that *prevents* an unhandled rejection. Path exclusion
rescues none of them — the residual outside test paths measured 75%, 100% and 85% wrong.

Two checks changed the outcome and are worth recording, because both looked safe enough to skip:
oxlint's `eslint/no-warning-comments` is `pedantic`, which poly enables, so a JS/TS `todo-marker`
would have been a straight duplicate of a rule already firing; and oxlint *implements*
`expect-expect` for jest and vitest, framework-aware and with an `assertFunctionNames` escape hatch
— strictly better than the pack rule proposed for the same job, whose false-positive rate measured
12/20. poly simply could not enable the plugin. The answer to #23 is therefore a `plugins` key on
the oxc engine, not a pack rule.

Python's `todo-marker` stays `off` despite measuring 0/20 false positives. Correctness was never the
objection: 38.5 findings per 1000 files, overwhelmingly deliberate tracked notes, and poly's own
`[lint.uncomment]` ships `remove_todos = false` precisely so they survive. Promoting it would redden
a well-maintained repository for documenting its own deprecation plan.

This sets the bar for the next candidate: a corpus row and a hand-read false-positive rate reported
as a fraction, before a severity, and the check that the tier-1 backend for that language does not
already cover it — run, not assumed.

## Alternatives considered

- **Ship the pack off by default, opt in via `[rules] builtin = true`.** Rejected: an empty
  ast-grep engine is what this ADR exists to fix, and a rule set nobody enables is the same hole
  with an extra config key in front of it. Individual *rules* still default off where the corpus
  says they should.
- **Write tier-1 crate backends for the uncovered languages.** Not available — no such crates
  exist for Swift or Kotlin — and out of proportion where they might be written. The pack does not
  block a later tier-1 port; a language that gains a native backend keeps its pack rules or drops
  them on the merits.
- **Vendor an upstream rule catalogue.** Rejected on licence grounds and on ADR 0003's "do not
  vendor". Note specifically that `eslint-plugin-sonarjs` >= 3.0 declares `LGPL-3.0-only` in its
  npm metadata while shipping under the Sonar Source-Available Licence, which forbids ports —
  `cargo deny` reads the SPDX identifier and would not catch it.
- **Put the rules in tier-2 (`treesitter.rs`) instead.** Rejected: tier-2 is deliberately
  grammar-generic, and per-language patterns are exactly what it is not. ast-grep already provides
  the per-language matching layer.

## Amendment — 2026-08-31: the shared test-context helpers cannot be extracted, so they are guarded

Five Rust rules — `unwrap-used`, `expect-used`, `placeholder-implementation`,
`blocking-call-in-async-fn`, and `undocumented-unsafe-block` — each carve test code out of their
matches with the same four `utils:` helpers (`test-attribute`, `cfg-test-attribute`,
`attribute-or-comment`, `in-test-context`), defined identically in every rule's own YAML rather
than once.

**Extraction into ast-grep's *global* `utils:` (one definition, five references) is the real fix,
and it is blocked on a deserializer poly-core does not have.** The helpers are relational-only
predicates, legal as a `utils:` entry and illegal as a top-level `rule:` — registering one as a
global util needs `parse_global_utils`, which needs `SerializableGlobalRule` deserialized from
YAML. `serde_yaml`, the deserializer ast-grep itself uses for that type, is unmaintained, and
swapping in a different YAML deserializer for types this dependent on `#[serde(flatten)]` and
untagged enums is not a change to make casually inside the pack's single-parse-path invariant (this
ADR's "One parse path" property).

So the five copies stay, as a known limitation rather than an oversight. What the pack does
instead is make the failure mode that actually matters — *silent* divergence between the five —
a test failure: `the_shared_rust_test_context_helpers_have_not_drifted`
(`crates/poly-core/src/engines/astgrep/pack.rs`) extracts each rule's `utils:` block and asserts
all five are byte-identical to the first. Four chances to drift, each turned into a build failure
instead of a corpus that quietly stops excluding test code in one of the five rules.

## Amendment — 2026-09-01: extraction was not blocked; the helpers are now global utils

The amendment above is **wrong on its central claim**, and it is worth recording why rather than
quietly deleting it.

Two premises were asserted from reading, not from checking:

1. *"A relational-only predicate is legal as a `utils:` entry and illegal as a top-level `rule:`."*
   It is legal as both. `deserialize_rule` raises `MissPositiveMatcher` only when a rule
   deserializes to **no** matcher at all; a relational rule (`inside:`, `follows:`) is itself a
   matcher and is pushed onto that list like any other.
2. *"Registering a global util needs a YAML deserializer poly-core does not have."* It needs
   exactly the one poly already used. `ast_grep_config` re-exports **`from_str`** — the same
   `serde_yaml`-backed entry point behind `from_yaml_string` — precisely so callers can deserialize
   its own types. poly had been calling it since the rule-test harness was written
   (`astgrep/test.rs`). No dependency was added, none swapped, and `serde_yaml` remains where it
   always was: a transitive dependency of ast-grep, accepted under the `RUSTSEC-2024-0436` entry in
   `deny.toml`, not something poly selects.

The generalisable error is that "unmaintained upstream dependency" was allowed to stand in for
"upstream API is unavailable to us". They are unrelated questions, and only the second one blocks.

**What ships instead.** The four helpers live once in
`crates/poly-core/src/engines/astgrep/builtin/utils.yml` and are referenced as
`rust-in-test-context`. The mechanism is general rather than pack-specific: a file named `utils.yml`
in *any* rule directory declares global utils, so a user rule set gets the same sharing the pack
does — which was the open request in the issue. Every `utils.yml` in a load merges into one
registration, so utils may cross-reference; ast-grep orders them topologically. Namespaces stay
separate: pack utils resolve only for pack rules, user utils only for user rules, so neither can
shadow the other. Rule-specific helpers (`blocking-call`, `preceded-by-safety-comment`) stay in
their rule's own `utils:` block, since sharing them would buy nothing.

**Why the drift guard was replaced rather than kept.** It compared five copies for equality; there
is now one definition, so equality is structural. What replaces it asserts the copies do not come
*back* — a reintroduced file-local `test-attribute:` would silently shadow the global for that one
rule, which is the same class of defect the original guard existed to catch.

**The verification gap this exposed.** The pack's own `*-test.yml` corpora were run only by hand,
via `poly rules test` against the builtin directory — so nothing in CI checked the semantics of the
26 rules a default `poly lint` runs. That is now `builtin_pack_passes_its_own_test_corpora`, landed
*before* the refactor so its result is a baseline rather than a claim. `ENGINE_VERSION` moves to
`builtin-pack-3`: the refactor is intended to preserve matching exactly, but a cache may not rely on
an intention, and the rules now compile through a different registration than any cached payload.
