# 0036 — Uncollected Test Files: A Path-Only Rule in the Quality Tier

- Status: Accepted
- Date: 2026-10-28

## Context

Goldziher/poly#39: a repository had 124 Python tests named `name.test.py` and `kebab-name.test.py`.
pytest's default `python_files` is `test_*.py` and `*_test.py`, so `pytest` collected none of them.
They ran only because a Taskfile invoked each file directly, and nothing flagged the drift. A green
test run cannot reveal a test that never ran, so this is a lint-time question by nature.

The check needs only the path: no parse and no content. The issue offered two homes, an ast-grep
pack rule or a path rule. An ast-grep rule matches nodes and has no way to report a file name. The
`quality` engine already holds the text-only rules (`file-too-long`, `lazy-ignore`), uses one flat
`[lint.quality]` / `[lint.<lang>.quality]` key scheme, and runs on every language.

## Decision

**Add `uncollected-test-file` to the `quality` engine** (`engines/quality/test_files/`), at warning
severity like every rule in the tier (ADR 0027).

A file is reported when its name marks it as a test but does not match what the language's runner
collects. The hard part is "marks it as a test", and the precision comes from these choices:

- **Dotted names count anywhere.** `parser.test.py` or `parser.spec.rb` (the stem ends in a test
  word after a `.`) is a convention only test files use.
- **Joined names count only inside a test directory.** `test-parser.py` or `parser_tests.py` count
  only under `test/`, `tests/`, `spec/`, `specs/` or `__tests__/`. Outside one the same shapes name
  tools: `scripts/test-snippets.py`, a `check-integration-tests.py`, gRPC's `soak_tests.go`.
- **A test directory alone is not enough.** `tests/factories.py` is a helper the runner is right
  to skip.
- **Test words are per language.** Python and Go count only `test`/`tests`, because `spec` there
  means a specification (`spec_types.go`, `spec-coverage.py`). Ruby, JS/TS and configured
  languages add `spec`/`specs`.
- **Go reads only the dotted shape.** Go has no test-directory convention: protobuf's
  `internal/testprotos/test/test_public.pb.go` is source.
- **Helpers are excluded.** That means helper words in the name (`helper`, `utils`, `setup`,
  `fixtures`, `conftest`, …) and helper directories (`fixtures/`, `testdata/`, `__mocks__/`,
  `support/`, …, plus `lib/`, `utils/`, `data/` below a test directory, so `src/lib/` stays source).
  A single-word stem (`tests.py`, `test.go`) is never a candidate.

Configuration uses flat keys, set on the per-language table:

- `uncollected_test_file`: the toggle. When unset, Python, Go and Ruby default on; everything else
  defaults off.
- `test_file_patterns`: globs that replace the runner defaults, so a custom pytest `python_files`
  can be matched. A pattern with `/` matches the whole forward-slash path; any other pattern
  matches the file name.
- `forbid_test_files`: flags every test-shaped file of the language (e.g. "shell tests must be
  `.bats`"; a `.bats` file is not Shell, so it is untouched).

Setting either of the last two switches the rule on for that language, so neither key can sit in a
config doing nothing. An invalid glob is a lint error on the file, not "matches nothing", which would
flag every test.

**Defaults on for Python, Go and Ruby; off for JavaScript/TypeScript; off with no patterns
elsewhere.** pytest, `go test` and RSpec/Minitest each have one dominant runner default. JS/TS does
not: Deno and `node --test` collect `*_test.ts`, which Jest and Vitest do not, so any built-in
default would be wrong for part of the ecosystem. The JS/TS profile still ships the Jest/Vitest
patterns for repositories that opt in. Shell and every other language have no runner default, so
the rule does nothing there until configured.

**No coverage claim.** `provides_language_lint` is unchanged. Judging a file's name is not
knowledge of its language.

The decision rests on measurement. A first cut that counted any test word anywhere flagged
`run_spec.go`, `spec_types.go`, `sh/interp/test_classic.go`, `scripts/test-snippets.py` and
`spec-coverage.py`, all false positives that shaped the rules above. The final rules were run
(`poly lint --no-workspace --only quality`) across the local corpus: every top-level repository
under `~/workspace` plus the `xberg-io/*` and `kreuzberg-dev/*` checkouts. All 272 findings land in
two checkouts of the repository from the issue, 136 each: 123 `*.test.py`, 8 `*-test.py` and 4
`test-*.py` under `scripts/tests/`, `tools/e2e/` and `charts/*/tests/`, plus one `*.test.rb`. poly's own tree reports
none. Python site-packages and the Go module cache, searched for the candidate shapes, turned up
no further false positives once the rules above applied.

ADR 0034 records that "the quality tier stays test-blind". That still holds for the metric rules: no
threshold changes inside a test. This rule's subject is the test file's name. It does not carve tests
out of anything.

## Consequences

Positive:

- The zero-collection case from the issue is caught at lint time. Python, Go and Ruby need no
  configuration.
- One rule covers custom runner configs (`test_file_patterns`) and "this language may not hold tests"
  policies (`forbid_test_files`) without a second mechanism.
- Path-only: no parse, so the cost per file is a few string comparisons, plus a glob match for
  candidates only.

Negative / risks:

- The candidate rules are heuristics. A repository that names test *scripts* `tests/test-foo.py` and
  runs them directly will see warnings. That is the issue's own case, and the fix is to rename or
  set `test_file_patterns`. Warning severity keeps CI green either way.
- Recall is lower than a "test word anywhere" rule: `src/parser_tests.py` outside a test directory
  is not reported. Precision was chosen over recall for an on-by-default rule.
- A `[lint.quality] test_file_patterns` set on the language-agnostic table applies to every
  language. The docs direct users to the per-language table. The engine does not reject the global
  form, because every other quality key is legitimately global.

## Alternatives considered

- **An ast-grep pack rule.** Rejected: ast-grep reports matches on nodes, not on file names.
- **A new cross-cutting engine.** Rejected: it would duplicate the `quality` engine's registration,
  cache key and flat-key config for one path check.
- **Read runner config (`pyproject.toml` `python_files`, `jest.config`) automatically.** Deferred.
  It means parsing several config formats, including executable ones (`jest.config.js`), on the
  per-file path. `test_file_patterns` covers the need explicitly.
- **On by default for JS/TS with the union of every runner's defaults.** Rejected: the union
  (`*_test.ts`, `test-*.js`, `__tests__/**`, Jasmine's `*Spec.js`, …) collects nearly everything,
  so the rule would be on and say almost nothing.
