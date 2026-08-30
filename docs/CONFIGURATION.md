# Configuration reference

Full reference for `poly.toml`. See the README's [Configuration](../README.md#configuration)
section for a quickstart; this file covers rule selection, suppression, monorepo cascading,
generated files, shared/remote config, the catalog tier, custom rules, code-quality metrics, and
comment removal.

poly discovers the nearest `poly.toml`, and `poly.local.toml` layers local overrides over the
primary config. In a monorepo, nested `poly.toml` files cascade — see
[Nested config in a monorepo](#nested-config-in-a-monorepo).

```toml
[defaults]
line_length = 120
line_ending = "lf"
final_newline = true
trim_trailing_whitespace = true

[discovery]
# Gitignore-style globs pruned from the file walk on every direct
# `poly lint` / `poly fmt` run (the CI and GitHub Action path), on top of
# `.gitignore` and the built-in vendored/generated prune set. The file-scoped
# `[hooks.builtin]` hooks (`lint`, `fmt`, `file_safety`) inherit these globs, so
# a repo states its excluded paths once.
exclude = ["test_apps/**", "docs/snippets/**", "artifacts/**"]

# Whether a file or directory named on the command line honors `exclude`.
# Defaults to `true`, which is what a hook — always handed explicit staged
# paths — needs. Set it to `false` to check named paths even when they match
# `exclude`; the directory walk still prunes them either way. `--force-exclude`
# and `--include-excluded` override this key for a single run, in either
# direction (flag beats config; the two flags are mutually exclusive).
force_exclude = true

# Directory names to keep despite the built-in prune set below, for a repo where
# one of those names is ordinary source rather than build output.
no_prune = ["build", "dist"]

# Whether poly lints and formats machine-generated files — those whose opening
# lines carry a `DO NOT EDIT` / `@generated` banner or a `<project>:hash:<digest>`
# stamp. Defaults to `true`: they are checked like any other file, which is how a
# generator bug gets noticed. Set it to `false` for a repo whose generated output
# is not its to fix; poly then reports each one as *skipped* rather than dropping
# it silently, so the count, the JSON payload and `--deny-skips` all still see it.
# `--skip-generated` / `--include-generated` override this for a single run.
generated = true

[fmt.python.ruff]
docstring_code_format = true
docstring_code_line_length = 120

[lint.python.ruff]
select = ["E", "F", "W"]

# Most linter backends accept uniform `select` / `extend_select` / `ignore`
# (rule codes, and category names where the backend has them): ruff, oxc, mago,
# rumdl, sqruff, biome, dockerfile, dotenv, ini and the ast-grep rule engine.
# `extend_select` adds to the defaults; `select` replaces them. `typos`,
# `quality` and `uncomment` take their own flat keys instead, and taplo's lint
# pass takes no options at all.
[lint.php.mago]
select = ["correctness", "security"]   # categories or rule codes
ignore = ["no-else-clause"]
php_version = "8.2"

# A per-rule `level` override works for every backend — poly applies it after
# linting, so it does not depend on the tool having its own severity config.
[lint.php.mago.rules.cyclomatic-complexity]
level = "warning"   # error | warning | info | hint

# Any *other* key in a `[rules.<id>]` table is forwarded to the backend as a
# tool parameter, where the backend supports it — today oxlint, rumdl and
# sqruff. ruff and mago accept `level` but ignore other per-rule keys; use their
# flat native keys instead (e.g. `mccabe_max_complexity` under
# `[lint.python.ruff]`). See ADR 0016.
[lint.javascript.oxc.rules.max-params]
max = 6

# Suppress specific rules per path glob (lint-only), across every backend.
[per-file-ignores]
"tests/**" = ["F401"]
"**/*.generated.php" = ["correctness"]

[hooks]
stages = ["pre-commit", "commit-msg"]

[hooks.builtin]
lint = true
fmt = true
commit = { stages = ["commit-msg"] }
file_safety = true
cargo = true
```

## Default rule selection

The Python (ruff) and JavaScript/TypeScript (oxlint) backends select rules beyond each tool's own
out-of-the-box default, widening coverage while keeping an unconfigured run green.

**Python (ruff)** selects `F`, `E4`, `E7`, `E9` (ruff's own default set) plus `W6`, `I`, `UP`, `B`, plus:

| Category | Codes | Covers |
|---|---|---|
| Typing | `ANN` | Every function signature carries type annotations. |
| Functional style | `SIM`, `C4`, `RET`, `FURB`, `PERF` | Comprehensions over map/filter, no needless else/assign-before-return, modern idioms, avoidable per-iteration work. |
| Error handling | `TRY`, `BLE`, `S110` | raise/except discipline, bare `except Exception:`, silently swallowed `try/except/pass`. |
| Complexity | `C90`, `PLR` | Cyclomatic complexity, Pylint's too-many-args/branches/returns/statements family. |
| Hygiene | `T20`, `TC`, `PTH`, `RUF`, `ARG` | Stray `print()`, imports that belong behind `TYPE_CHECKING`, `os.path` calls pathlib does better, ruff's own rules, unused arguments. |

**JavaScript/TypeScript (oxlint)** ran only the `correctness` category by default; it now also
enables `suspicious`, `pedantic`, and four named rules: `typescript/no-explicit-any`,
`typescript/no-non-null-assertion`, `no-console`, and `complexity`. The last sits in oxlint's
(off) `restriction` category, but its default threshold is exactly 20 — the same cyclomatic-
complexity budget poly applies to every other language — so JS/TS gets the metric from oxlint
rather than a separate implementation.

**The added rules are guard rails, not gates.** `poly lint` exits non-zero only on error-severity
findings. The correctness core each tool always ran — ruff's `F`/`E4`/`E7`/`E9`/`W6`/`I`/`UP`/`B`
and oxlint's `correctness` category — keeps error severity, so real defects still fail CI exactly
as before. Every newly-added category reports at **warning** and never fails a run on its own.
Promote one you want enforced with a per-rule override:

```toml
[lint.python.ruff.rules.ANN201]
level = "error"
```

**Rules held back.** A handful of rules stay off even though their category is selected — each was
measured against a multi-repo corpus and found to fire on legitimate code rather than a defect:

| Rule | Category | Why it's off |
|---|---|---|
| `B008` | flake8-bugbear | Flags the FastAPI/typer `Depends(...)` pattern — a deliberate call in a default. |
| `RUF100` | ruff | Fires on any `# noqa` code poly does not select; measures config distance, not code quality. |
| `ANN002` / `ANN003` | flake8-annotations | Only `Any` ever satisfies them, which `ANN401` (on by default) then flags anyway. |
| `ARG002` | flake8-unused-arguments | Fires on fixed-signature overrides/callbacks (`*args`/`**kwargs`). |
| `PLR2004` | Pylint | Overwhelmingly HTTP status codes in test assertions. |
| `TRY003` | tryceratops | Demands a dedicated exception subclass for every `raise ValueError("…")`. |

Re-enable any of them with `extend_select = ["<code>"]` under `[lint.python.ruff]`. The `EM`
category (exception message assigned to a variable before `raise`) is not selected at all.

For oxlint, `no-underscore-dangle`, `max-lines-per-function`, `max-lines`, and
`max-classes-per-file` are turned back off for the same reason (a universal private-field
convention; `describe()` blocks that always exceed the 50-line default; a 300-line-per-file
default poly does not endorse elsewhere; and a one-class-per-file rule that fires on error
taxonomies, test mocks, `.d.ts` stubs, and cohesive module groups rather than defects).
Re-enable with `extend_select` under `[lint.javascript.oxc]` / `[lint.typescript.oxc]`.

**Migration note.** Selecting `C90`/`PLR` makes the already-existing `mccabe_max_complexity`,
`pylint_max_args`, `pylint_max_branches`, and `pylint_max_returns` options under `[lint.python.ruff]`
take effect for the first time. A repo that previously set `mccabe_max_complexity = 10` and saw no
effect now gets `C901` findings — at warning severity, so it won't fail CI unless promoted.

**Formatter layering.** For the CSS/SCSS/Less (malva), HTML/Vue/Svelte (markup_fmt), GraphQL and
YAML backends, `[defaults] line_length` and `[defaults] line_ending` supply `print_width` and
`line_break` only when you have not set those keys yourself in the engine's own table — your
config is always the top layer. Some tools expose no such setting at all: Nix (alejandra) and
Ruby (rubyfmt) are zero-configuration formatters, so `[fmt.nix.alejandra]` and
`[fmt.ruby.rubyfmt]` declare no keys at all — anything written there is reported as
`unknown-config-key`; TOML (taplo) always trims
trailing whitespace regardless of `[defaults] trim_trailing_whitespace`.

**Unknown or misconfigured keys are reported.** `poly` reports unknown config keys, unknown
top-level sections, and wrongly-typed values as warnings, so a misspelled key no longer parses
silently and does nothing.

**PHP (mago).** Mago ships six default-enabled rules at error level that measure a *metric* rather
than detect a defect — `cyclomatic-complexity` (>15), `excessive-parameter-list` (>5),
`too-many-methods` (>10), `too-many-properties` (>10), `too-many-enum-cases` (>20), and
`kan-defect`. poly reports these at **warning**, so a complexity budget never fails CI, while
mago's correctness, safety, and security rules (`no-ffi`, `no-eval`, `no-literal-password`,
`tainted-data-to-sink`, `no-unsafe-finally`, `no-empty`, …) keep error severity. Promote one back
with:

```toml
[lint.php.mago.rules.cyclomatic-complexity]
level = "error"
```

## Suppressing a rule inline

`[per-file-ignores]` is the right tool for a whole file or a class of files. For a *single*
justified exception inside an otherwise-normal file, write the directive in the file itself, in
that language's own comment syntax (see [ADR 0028](../adrs/0028-inline-suppression-directives.md)):

```python
import os  # poly: allow[F401] re-exported for backwards compatibility
```

```typescript
// poly: allow[no-debugger] deliberate breakpoint, stripped from the release bundle
debugger;
```

- **`poly: allow[RULE, RULE2] reason`** — covers the line it trails. On a line that is *entirely*
  a comment it covers the next non-blank line instead.
- **`poly: allow-file[RULE] reason`** — covers the whole file, wherever in the file it appears.
- Rule codes are comma-separated and matched exactly or as a code family prefix, the same way
  `[per-file-ignores]` matches them: `allow[F]` covers `F401`, but not `FOO1`. `allow[*]` covers
  every rule.
- It works for every backend, because it is applied centrally by the runner — ruff, oxlint,
  typos, the tree-sitter tier, and any backend added later, with no per-engine wiring.

**A reason is mandatory.** A directive with nothing but whitespace or punctuation after the
closing bracket does **not** suppress anything; the rule still fires and poly additionally reports
a `lazy-ignore` warning on the directive line.

poly recognizes the directive after any of the comment openers `//`, `#`, `--`, `;`, `/*`, `*`,
`<!--`, `%`, `!`, `dnl`, and `rem` — no per-language configuration. Quotes are deliberately not
openers, so a directive-shaped string literal never suppresses.

## Nested config in a monorepo

Run `poly` from a monorepo root and each sub-project's `poly.toml` cascades over the root, the
way ruff and eslint resolve config (see [ADR 0018](../adrs/0018-hierarchical-configuration.md)). A
nested config declares **only the diff** — it inherits `[defaults]`, the `[lint.*]`/`[fmt.*]` rule
tables, and `[per-file-ignores]` from its ancestors, up to the workspace root:

```toml
# repo/poly.toml — the workspace root
[workspace]
root = true            # stops the upward cascade here (a repo's `.git` dir is
                       # an implicit boundary too, so this is optional in a repo)

[defaults]
line_length = 120

[lint.python.ruff]
select = ["E", "F", "W"]
```

```toml
# repo/frontend/poly.toml — governs repo/frontend/** only
[defaults]
line_length = 100      # overrides the root; ruff select is inherited

[per-file-ignores]
"*.spec.ts" = ["no-console"]   # glob is relative to repo/frontend/
```

Resolution rules:

- **Which config governs a file does not depend on how you invoked poly.** `poly fmt .`, `poly fmt
  frontend`, and `poly fmt frontend/src/app.ts` all apply `frontend/poly.toml` to that file — so a
  pre-commit hook, which is always handed explicit staged paths, gates on exactly what a whole-repo
  run gates on.
- **Rules and defaults cascade** (root → child, deep-merged; the nearest config wins).
- **`[discovery] exclude` globs are additive** across the tree — each config's excludes prune its
  own subtree, so a parent exclude already covers its children.
- **`[per-file-ignores]` globs are relative** to the directory of the config that declares them.
- `--config <path>` pins one config for the whole run and bypasses nested resolution.

## The built-in prune set

Independently of `exclude`, poly never walks into a directory with one of these names, at any depth:

```text
node_modules  vendor  deps    target  dist   build  .git
.venv         venv    .tox    .gradle .next  .nuxt  coverage
__pycache__   .mypy_cache      .ruff_cache    .pytest_cache  .polylint
```

These hold vendored code, build output, or tool caches, and they are frequently *tracked*, so
`.gitignore` alone does not exclude them. Every run reports what this pruned, so a directory you did
not expect to lose is visible rather than silently missing:

```console
$ poly fmt --check .
All formatted. (2239 file(s) checked, 26 director(ies) skipped by the built-in prune set)
  26 director(ies) skipped by the built-in prune set (e.g. src/cli/pipeline/commands/build, node_modules)
  these were not walked, so the files inside them are not counted; keep one with [discovery] no_prune
```

`build` and `dist` are build-output conventions in most ecosystems and ordinary domain nouns in
some. Where one of them is real source, name it in `no_prune`:

```toml
[discovery]
no_prune = ["build", "dist"]
```

These are bare directory names, not globs — the built-in set is a name list and `no_prune` subtracts
from it. To prune *more* paths, use `exclude`. Unlike `exclude`, `no_prune` replaces rather than
accumulates across config layers, and it is read from the run's root config only: a `poly.toml`
*inside* a pruned directory cannot un-prune it, because that directory was never walked to find the
config in the first place.

## Machine-generated files

A file whose opening lines carry a `DO NOT EDIT`, `@generated` or `Code generated …` banner is
**linted, formatted and fixed like any other file**. That is deliberate: a generator can emit a
defect, and poly reporting it is how anyone finds out. All three phases agree — there is no shape
of file that `poly fmt` reformats but `poly lint --fix` refuses to touch.

The one exception is narrower than a banner and is a correctness guard, not a preference. When the
header stamps a **content hash** over the body — `<project>:hash:<digest>`, the shape a generator
later verifies — poly still reports on the file but never writes to it. Reformatting the body
invalidates the hash, so the generator's verify step reports drift on a file no human touched and
the only remedy is a regen that throws the change away. `poly fmt` reports those as
`skipped … hash-stamped generated file`, `poly lint --fix` reports the diagnostics and says how
many fixes it withheld, and `--fix-generated` opts back in.

To keep generated files out of poly entirely — the repo whose generated output is not its to fix —
turn them off once, for both phases:

```toml
[discovery]
generated = false
```

They are then **reported as skipped**, not silently dropped: they stay out of the `N file(s) linted`
count, they appear in the `--format json`/`toon` payload, and they count against `--deny-skips` /
`--max-skips`, so a gate cannot quietly stop covering a tree.

```console
$ poly lint --verbose .
Nothing was linted. (0 file(s) linted, 1 skipped (machine-generated file ([discovery] generated = false)))
  skipped bindings/api.py: machine-generated file ([discovery] generated = false)
```

The key is read from the config nearest each file, so a nested `poly.toml` (see
[Nested config in a monorepo](#nested-config-in-a-monorepo)) can opt out one subtree without touching the
rest. `--skip-generated` and `--include-generated` override it in either direction for a single
run, and beat every config in the tree.

## Sharing configuration

A top-level `extends` list inherits any section of `poly.toml` — `[defaults]`,
`[lint.*]`/`[fmt.*]`, `[tools.*]`, `[per-file-ignores]`, `[hooks.*]`, and so on — from local
or pinned remote base configs, so an org can maintain one baseline instead of copy-pasting
it into every repo (see [ADR 0020](../adrs/0020-shared-remote-configuration.md)). Entries use
the same `path`/`git`/`revision` vocabulary as `[[hooks.sources]]`:

```toml
extends = [
  { git = "https://github.com/acme/poly-baseline", revision = "<40-hex-oid>", file = "poly.toml" },
  "./poly.overrides.toml",   # later entry = higher precedence
]
```

Bases are deep-merged underneath this file, in listed order; this `poly.toml` and then
`poly.local.toml` always win on top.

**`exclude` lists accumulate; every other key replaces.** A repo that adds one glob of its own
keeps every glob it inherited — and keeps receiving later changes to the base — instead of having
to restate the base's list and freeze a copy of it:

```toml
# base: [discovery] exclude = ["vendor/**", "target/**"]
extends = ["../baseline/poly.toml"]

[discovery]
exclude = ["generated/**"]   # effective: vendor/**, target/**, generated/**
```

To drop what you inherited and state the whole list yourself, add `exclude_mode = "replace"` next
to the `exclude` in that table. The same rule governs `[discovery] exclude` → `[hooks.builtin.*]`
inheritance, and it applies to `exclude` only — rule selections, `[rules] dirs`, `clippy_args` and
every other array still replace.

A `git` base pinned to a full commit OID needs no lock;
a branch or tag ref requires running `poly config update` first, which resolves it into
`poly-config.lock` and prints the `[hooks]`/`[tools]` the base introduces. `extends` is
forbidden in `poly.local.toml`. Extending a remote base means trusting that repository to
run code on your machine — treat it like any other dependency.

`poly config show` prints the effective, fully-merged result — every layer applied, every key as
poly resolved it — so `diff`ing it against your own `poly.toml` answers "what did poly actually
keep?". See the [CLI reference](CLI.md#config--what-did-poly-actually-parse) for its output
formats.

## Optional catalog tools

Opt into tools from the embedded mdsf catalog only when you want them:

```toml
[tools.prettier]
enabled = true
files = "**/*.{js,ts}"

[tools.black]
enabled = true
files = "**/*.py"
```

Catalog tools are capability-probed on `PATH`; a missing binary is skipped instead of making the
whole run fail. See [Backend coverage](BACKENDS.md) for the full catalog of 348 tools.

## Custom rules

Write your own lint rules — and codemods — as [ast-grep](https://ast-grep.github.io) YAML,
in any of the 300+ languages poly can parse. Custom rules run in-process alongside the native
backends on every `poly lint`, and `poly lint --fix` applies any `fix:` rewrites they declare.
No plugin, no fork, no extra toolchain: rules run on the same tree-sitter grammars poly already
bundles.

Point `[rules] dirs` at one or more directories of rule files (paths are resolved relative to the
`poly.toml` that declares them, so a rule set works from any working directory):

```toml
[rules]
dirs = [".poly/rules"]   # default; set to [] to disable custom rules
```

Each rule is a standard ast-grep YAML document. The `language:` field names a tree-sitter
grammar; any metavariable used in `fix:` must be bound by the `rule:` pattern:

```yaml
# .poly/rules/python/use-is-none.yml
id: use-is-none
language: python
severity: warning
message: Use `is None` rather than `== None`.
rule:
  pattern: $X == None
fix: $X is None
```

For languages where a bare fragment is not valid at file top level (e.g. Go), use ast-grep's
`context`/`selector` pattern form.

### Testing rules

A rule may ship a companion `<name>-test.yml` holding `valid` snippets (must **not** match) and
`invalid` snippets (must match). An `invalid` entry can also assert the rule's **autofix output**
by giving `code` + `fixed` instead of a bare string:

```yaml
# .poly/rules/python/use-is-none-test.yml
id: use-is-none
valid:
  - x is None
invalid:
  - x == None                 # must match; fix output unchecked
  - code: result == None      # must match AND autofix to `result is None`
    fixed: result is None
```

Run the checks with `poly rules test` (exits non-zero on any failed snippet), and list the
resolved rules with `poly rules list`. Both default to the configured `[rules] dirs`, or accept
explicit directories as arguments. `poly rules list` covers poly's **built-in rule pack** (26
rules across C#, Elixir, Go, Java, Kotlin, Python, Ruby, Rust and Swift — ADR 0029) as well as
your own rules, marking each row `builtin` or `user`, and reflects the config that governs
them — `[rules] builtin = false`, `[lint.astgrep]` `select` / `extend_select` / `ignore`, and
`[lint.astgrep.rules.<id>] level`.

## Code quality metrics

`poly lint` measures a handful of structural properties straight off the tree-sitter parse, for
languages that have no linter of their own as much as for those that do. Every finding is a
**warning**, so none of them fail CI on their own.

| Rule | Default | On by default |
| --- | --- | --- |
| `file-too-long` | 1000 lines | yes |
| `function-too-long` | 80 lines | yes |
| `type-too-long` | 300 lines | yes |
| `too-many-parameters` | 6 | yes |
| `nesting-too-deep` | 4 | yes |
| `cyclomatic-complexity` | 20 | yes |
| `lazy-ignore` | — | yes |
| `magic-number` | allows `-1, 0, 1, 2, 10, 100` | no |
| `law-of-demeter` | depth 3 | no |

`lazy-ignore` reports a suppression written for *another* tool with no reason attached — a bare
`# noqa` or `// biome-ignore` with nothing after the colon, or an `// eslint-disable*` /
`// oxlint-disable*` with nothing after the conventional `--` separator. Rust's `#[allow(..)]` is deliberately **not** among them: it belongs to the built-in
ast-grep rule `allow-attribute-without-reason`, which reads `reason = "..."` and a preceding
comment correctly and ships off by default. Opt in with
`extend_select = ["allow-attribute-without-reason"]`.

```toml
[lint.quality]
function_too_long_lines = 120      # raise the budget everywhere
magic_number = true                # opt in to a rule that ships off

[lint.go.quality]
function_too_long_lines = 200      # per-language override wins
```

The structural rules need a grammar poly holds a construct table for: **Python, Rust, Go,
JavaScript, TypeScript, TSX, Java, Kotlin, C, C++, C# and Ruby**. A language poly can parse but
not model — Zig, Swift, Dart, Gleam, Elixir, PHP, Nix, Scala, Lua, R — gets the file-length and
ignore-marker checks only, and still reports `no lint rules for <language>`: counting lines is
not knowledge of a language, and poly will not claim it is.

Where a backend already reports the same measurement, poly defers to it rather than reporting it
twice — Python keeps ruff's `C901` for complexity, JavaScript and TypeScript keep oxlint's
`max-depth`. See [ADR 0027](../adrs/0027-code-quality-tier.md).

## Comment removal (opt-in)

The `uncomment` backend strips comments across every language it recognizes, guided by
tree-sitter and a set of preservation rules (shebangs, `~keep`, TODO/FIXME, documentation, and
your own patterns). It is a **lint** backend: `poly lint` reports each removable comment block as
a warning (which never fails CI), and `poly lint --fix` removes them. By default it reports only
comments that look like *commented-out code*; set `code_only = false` to report every removable
comment.

It is **off by default**. Enable it, and tune what it keeps, with a language-agnostic
`[lint.uncomment]` block plus optional per-language overrides:

```toml
[lint.uncomment]
enabled = true              # required — the backend is opt-in
remove_todos = false        # keep TODO comments (default)
remove_fixme = false        # keep FIXME comments (default)
remove_docs = false         # keep documentation comments / docstrings (default)
use_default_ignores = true  # keep the built-in directive allow-list (default)
code_only = true            # only report comments that look like commented-out code (default)
preserve_patterns = ["HACK", "NOTE"]  # keep comments containing these substrings

# Per-language override: strip Python docstrings but keep them elsewhere.
[lint.python.uncomment]
remove_docs = true
```

Per-language booleans override the global value; `preserve_patterns` are unioned with the global
list. A language `uncomment` does not recognize is simply left untouched.
