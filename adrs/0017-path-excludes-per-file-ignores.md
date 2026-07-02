# 0017 — Path Exclusions and Per-File Rule Ignores

- Status: Accepted
- Date: 2026-07-01
- Updated: 2026-07-02 (hierarchical config, ADR 0018: with nested `poly.toml`s,
  `[discovery] exclude` globs are unioned across the tree, each rooted at its own
  config directory, and `[per-file-ignores]` globs are resolved relative to their
  owning config's directory rather than only the run root.)

## Context

polylint needs two orthogonal mechanisms to avoid linting files and suppressing rules:

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
