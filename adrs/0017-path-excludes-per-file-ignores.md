# 0017 — Path Exclusions and Per-File Rule Ignores

- Status: Accepted
- Date: 2026-07-01
- Updated: 2026-07-02 (hierarchical config, ADR 0018: with nested `poly.toml`s,
  `[discovery] exclude` globs are unioned across the tree, each rooted at its own
  config directory, and `[per-file-ignores]` globs are resolved relative to their
  owning config's directory rather than only the run root.)
- Updated: 2026-08-12 (the file-scoped `[hooks.builtin]` hooks — `lint`, `fmt`,
  `file_safety` — inherit `[discovery] exclude` instead of restating it, under the
  accumulate rule ADR 0020 defines for `exclude`; a hook opts out with
  `exclude_mode = "replace"` in its own table.)
- Updated: 2026-08-12 (exclude anchoring is stated explicitly below and surfaced by
  a `poly doctor` check; the matching semantics themselves are unchanged.)
- Updated: 2026-08-29 (line-scoped suppression: `// poly: allow[rule-id] reason` inline
  directives — see ADR 0028 — supplement these file-level globs for exceptions too narrow to
  justify disabling a rule for a whole file.)
- Updated: 2026-08-30 (machine-generated files: one `[discovery] generated` opt-out spanning
  `lint`, `fmt` and `--fix`, and `lint --fix` aligned with `fmt` on which files it declines to
  rewrite — see the amendment below.)
- Updated: 2026-08-31 (cross-reference: the generated-file skip counting against `--deny-skips`
  below is the file-limitation half of ADR 0031's charged/uncharged split — see the note after
  the amendment.)

## Exclude anchoring

`exclude` globs are gitignore patterns, so:

> **A glob that does not begin with `/` matches a file or directory of that name at
> any depth below the config that declared it; a leading `/` anchors it to that
> config's directory.**

The unanchored form is what people write by default, and it is the one that
surprises:

```toml
[discovery]
# The top-level e2e/ — and also src/test/java/io/xberg/e2e/, because `e2e` is an
# ordinary Java/Kotlin package name.
exclude = ["e2e/**"]

# Only the top-level e2e/ directory.
exclude = ["/e2e/**"]
```

Poly derives a bare `foo` rule from every `foo/**` pattern (without it the
directory itself is not pruned and the walk descends into a tree the user asked to
skip), and that derived rule carries no separator — which is what makes `e2e/**`
reach every depth. Nothing fails when it over-reaches: the extra files are simply
never discovered, and the run reports a clean pass over what remains. A consumer
lost months of formatting coverage on two `test_apps/` trees this way.

**The semantics are deliberately left alone.** Silently re-anchoring existing
`exclude` entries would change which files a repository's gate covers without
anyone editing a line, which is a worse failure than the papercut. Instead the rule
is documented on `--exclude`, on `[discovery] exclude`, and on the MCP `exclude`
parameter, and `poly doctor` warns when a rule prunes directories at more than one
depth — naming the directories, since the whole value is showing the author the
tree they did not know they were hiding.

## Context

poly needs two orthogonal mechanisms to avoid linting files and suppressing rules:

1. **Whole-file skips:** some files should never be linted or formatted — generated code
   (protobuf stubs), lock files (Cargo.lock, package-lock.json), vendor directories. Users
   expect gitignore-style globbing.
2. **Selective rule suppression:** a well-formatted file still triggers style rules the repository
   wants suppressed in specific contexts — test utilities might ignore complexity checks, generated
   SQL from templates might ignore security rules, old code awaiting refactor might silence
   deprecation warnings.

The challenge is unifying these across ~15 backends with divergent comment-syntax and
suppression APIs (ruff's `noqa`, oxc's `// eslint-disable`, sqruff's `-- noqa`).

## Decision

- **Path `exclude`: unified gitignore-style globs.** Available in three places:
  - `[discovery] exclude` in the config file (permanent, version-controlled).
  - A repeatable `--exclude <glob>` CLI flag.
  - An MCP `exclude` parameter (e.g. from the poly GitHub Action or editor integration).
  These are merged once before discovery (via `filter::merged_excludes()`) and passed to
  `discover()` as the exclusion set. No file matching an exclude glob is discovered.
- **`[per-file-ignores]`: cross-engine rule suppression.** A TOP-LEVEL table in `poly.toml`
  (not nested under `[lint]`, to avoid collision with the `[lint.<lang>.<tool>]` slicing) that
  maps gitignore-style path globs to arrays of rule codes. Example:

  ```toml
  [per-file-ignores]
  "tests/**" = ["F401", "too-many-methods"]
  "*.gen.py" = ["E501"]
  ```

  Matching semantics: a file matches a glob if the glob pattern matches its relative path
  (forward-slash normalized, rooted at the run base). Matching is evaluated once per file,
  not per diagnostic.
- **Rule matching is exact or prefix-based:** A rule code matches a suppression rule when
  it is an exact match (`F401` suppresses `F401`) or a prefix match where the next character
  is non-alphabetic (ruff-style rule families). Thus `F` suppresses `F401` but not `FOO`,
  and `too-many` suppresses `too-many-methods` but not `too-many-args` if the latter were
  a separate code. This prevents short codes from silently swallowing unrelated codes from
  other engines.
- **Suppression timing: before the fix loop.** Per-file-ignore filtering is applied as a
  post-lint filter (after engines report diagnostics) but BEFORE the `--fix` rewrite loop.
  This ensures `--fix` never silently rewrites a file for a rule the user has configured to
  ignore — keeping the user's intent explicit and debuggable.
- **Engine-agnostic filtering:** The normalized `Diagnostic.code` is matched; the filter
  does not care which engine produced the diagnostic. A `[per-file-ignores]` entry suppresses
  matching codes across all backends uniformly.

## Consequences

Positive:

- Two clear, orthogonal levers: exclude globs control discovery (fast path), per-file-ignores
  control post-lint filtering (semantic suppression).
- Uniform syntax: users learn gitignore-style glob patterns once and apply them everywhere.
- Cross-engine: one `[per-file-ignores]` table replaces per-tool comment syntax (ruff `noqa`,
  oxc `eslint-disable`, sqruff `noqa`), making it possible to suppress a rule from any backend
  in one place.
- Explicit and debuggable: suppressed diagnostics are still computed and filtered, not skipped
  in silence. Users can run with `--debug` or temporarily remove an ignore to understand what
  rules would fire.

Negative / risks:

- Per-file-ignores is top-level, not scoped to a language or tool, so all rule codes are in one
  namespace. Code collisions are unlikely (tools are carefully chosen to avoid overlapping codes),
  but documentation must clarify which tool owns which code.
- Glob complexity: invalid or overly broad globs (e.g. `**` with no prefix) can suppress more
  files than intended. Invalid globs are logged as warnings and skipped, not failures.
- The filtering happens in-memory after engines run, so it does not reduce the cost of analysis.
  It is a *suppression* mechanism, not a *skipping* mechanism. For large repositories, `exclude`
  at discovery time is the performance lever; `[per-file-ignores]` is for semantic control.

## Alternatives considered

- **Per-tool suppression tables:** rejected — forces users to restate the same suppression
  (~15 times) for each backend. Per-file-ignores is unified precisely to avoid duplication.
- **Engine-specific comment syntax only (no `[per-file-ignores]`):** rejected — expects users to
  learn multiple comment styles and restricts suppression to sources they control (cannot suppress
  generated files without patching the generator). The config table is simpler and applies
  uniformly.
- **Nested under `[lint]` instead of top-level:** rejected — would collide with the slice model
  `[lint.<lang>.<tool>]`, where `[lint.per-file-ignores]` would be parsed as language
  "per-file-ignores". Top-level placement is the only collision-free option given the config
  hierarchy.
- **Glob matching at discovery time instead of post-lint:** rejected for `[per-file-ignores]` —
  the goal is semantic suppression (silence specific rules), not file skips. A file may trigger
  different rules in different runs (e.g. after a refactor); post-lint filtering lets users
  suppress by rule, not by file. Path `exclude` does match at discovery time (the fast path),
  and that is the right choice for whole-file skips.

## Amendment — 2026-08-30: machine-generated files

Generated files were the one whole-file skip ADR 0017 named (§Context, item 1) and never gave a
lever for, so three code paths had grown three different answers to "is this file poly's to act
on?":

| phase | question it asked | effect |
| --- | --- | --- |
| `lint` | none | always reported |
| `fmt` | is there a **content-hash stamp**? | skipped only if stamped |
| `lint --fix` | is there **any generated marker**? | rewrite withheld on a bare banner too |

The third is the one that was wrong. A `DO NOT EDIT` banner announces provenance; it makes no
claim about the bytes. `poly fmt` therefore reformatted a banner-only generated file — correctly,
since not checking a file is worse than formatting one — while `poly lint --fix` refused a
one-character autofix in the same file in the same repository, and reported a "generated file(s)
not fixed" line for it. Nothing justified the split, and the asymmetry read as a bug in whichever
command the user ran second.

**`lint --fix` now asks the same question `fmt` does.** A rewrite is withheld only from a header
that stamps `<project>:hash:<digest>` over the body. That guard stays, in both phases, and is not
a noise preference: reformatting a stamped body invalidates the hash, the generator's verify step
then reports drift on a file no human touched, and the remedy is a regen that discards the
formatting — a loop, in which one reporter had 110 of 123 files. `--fix-generated` remains the way
out of it, and is now *only* that; it has nothing left to say about banner-only files, since those
are rewritten by default.

**`[discovery] generated`** (default `true`) is the opt-out, for a repository whose generated
output is not its to fix. It is one key covering `lint`, `fmt` and `--fix`, because "is this file
mine to check?" has one answer per file; two keys under `[lint]` and `[fmt]` would be two things a
reader has to keep in agreement to get the behaviour they asked for. It sits in `[discovery]`
alongside `exclude`, `force_exclude` and `no_prune` — the table that already answers *which files
poly acts on*, and already carries the built-in vendored/**generated** prune set that `no_prune`
opts out of.

Rejected placements: `[lint] generated` (lint-only namespace — `fmt` reads `[fmt]`, so a key
governing both phases cannot live in either); a paired `[lint] generated` + `[fmt] generated` (two
keys that must agree, and silently diverge when only one is set); a new top-level `[generated]`
table (a whole section for one boolean, inviting `markers = [...]` scope creep that the narrow,
deliberately un-tunable marker list in `filter/generated.rs` exists to avoid); `[defaults]` (style
values, not file selection).

Unlike its neighbours in `[discovery]`, this one **cannot prune the walk** — a banner is content,
so the file must be read before the question can be answered. It is a skip, not an exclusion, and
that is load-bearing: an opted-out file is reported through the existing `SkippedFile` machinery
(`machine-generated file ([discovery] generated = false)`), so it stays out of the `checked` count,
appears in the `json`/`toon` payload, and counts against `--deny-skips` / `--max-skips`. Dropping
those files silently would let a gate stop covering a tree with nothing going red — the failure
mode this ADR's own skip accounting exists to prevent.

**Cross-reference (2026-08-31):** the generated-file skip counting against `--deny-skips` /
`--max-skips` above is a claim about a *file* poly declined, not about the *config key* that
declined it — `[discovery] generated = false` is written by the repository on purpose, exactly
like `enabled = false` on an engine, yet the skip it produces is charged. Read next to ADR 0031's
engine-level `enabled = false` (never charged), the two can look like they contradict: both are
instructions the caller wrote. They do not — ADR 0031 charges by what the reason is *about*, not by
who wrote the config line. A charged reason names a file poly could not verify (generated, binary,
no engine, no rules); an uncharged reason names an *engine* the caller withdrew (`--only`/`--skip`,
`enabled = false`) from a file poly could otherwise have checked. See ADR 0031 for the full split.

Per-config, not root-only (unlike `force_exclude` and `no_prune`): a nested `poly.toml` may opt out
its own subtree. No per-language form — the marker scan is language-agnostic, and `[lint.<lang>]`
holds only `<tool>` sub-tables, so a scalar written there would be read by nothing.
`--skip-generated` / `--include-generated` override the key in either direction for one run, and
beat every config in the tree; an override a nested config could veto would not be an override.

Cost on the default path is one indexed `bool` load per file: the flag is resolved once per config
before the `par_iter`, and tested before the content scan, so a run that has not opted out never
looks at a file header for this.
