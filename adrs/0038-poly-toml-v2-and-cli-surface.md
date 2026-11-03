# 0038 — poly.toml v2 and the CLI Surface

- Status: Accepted
- Date: 2026-11-03

## Context

Epic #48 splits poly into a small core and downloadable engine components. That changes what a
user configures and what a run can do: an engine may now live in a component that is not
installed, and a run may need the network. The v1 config and CLI were shaped for one bundled
binary and do not express that:

- Engines are configured as `[lint.<lang>.<tool>]` and `[fmt.<lang>.<tool>]` (ADR 0016). The same
  engine appears under every language and both phases, so a ruff option is repeated up to four
  times, and nothing in the config names the component an engine ships in.
- `[rules]` configures ast-grep, but is a top-level table that looks unrelated to the
  `astgrep` engine it feeds (ADR 0029).
- Workspace behaviour is spread over `[workspace] root`, `[lint] workspace` and the
  `--workspace` / `--no-workspace` flag pair (ADR 0018).
- Two lock files, `poly-config.lock` (ADR 0020) and `poly-hooks.lock`, are updated by two
  commands, `config update` and `hooks update`.
- `--no-color`, `--force-exclude`, `--include-generated`, `--skip-generated`, `fmt --check` and
  `--exit-zero-on-fix` are either booleans standing in for a three-way choice or redundant with
  a default.
- There is no way to say "never touch the network" or "what happens when a tool is missing".

Because the config has to change anyway, this ADR takes the one break the epic allows and fixes
the surface in one step rather than in several releases. It amends ADR 0006 (the canonical
`poly.toml`), ADR 0013 (`[tools]` and the catalog tier's lock and install policy), ADR 0016
(`[lint|fmt.<lang>.<tool>]` and its rule-selection keys) and ADR 0018 (the `[workspace]` and
`[lint] workspace` keys and nested-config resolution). The component mechanism itself is
decided in the companion ADR on downloadable engine components.

### Decision drivers

- A user configures an engine, not a component. The config must work the same whether the
  engine is bundled or downloaded.
- One place per setting. A key that exists in two tables or under two spellings is a bug
  magnet.
- Nothing downloads unless a run needs it or the user asks. The strict rule must be visible in
  the CLI, not only in documentation.
- A missing component must never silently change output between machines.
- Config validation (`config validate`, `config show`, `doctor`) must work offline with nothing
  installed.
- Old configs must fail loudly and be convertible mechanically, with comments preserved.

## Decision

poly 2.0 changes the config schema and the CLI surface together. A v1 config is a hard error
with a migration hint; there is no compatibility mode.

### Engine-first config

**`[engines.<id>]` replaces `[lint.<lang>.<tool>]` and `[fmt.<lang>.<tool>]`.** One table per
engine id (the `Engine::name()` of ADR 0016, for example `ruff`, `oxlint`, `typos`, `astgrep`).
Options that apply everywhere sit directly in the table. Narrowing is by sub-table:

```toml
[engines.ruff]                    # all phases, all languages
select = ["E", "F"]

[engines.ruff.format]             # one phase
line_length = 100

[engines.ruff.lang.python]        # one language
target_version = "py312"

[engines.ruff.rules.ANN201]       # per-rule override (ADR 0016)
level = "error"
```

Precedence, lowest to highest: tool defaults, then the tool's native config files (ADR 0007),
then `[engines.<id>]`, then its phase sub-table, then its language sub-table, then the
language-and-phase sub-table (`[engines.ruff.lang.python.format]`). ADR 0016's rule-selection
keys (`select`, `extend_select`, `ignore`, `rules.<id>.level`) keep their meaning and move with
the table. `[tools.<name>]` (ADR 0013, ADR 0035) is unchanged: the catalog tier is not an engine
and does not live in a component.

**`[rules]` becomes `[engines.astgrep]`.** `rule_dirs` and `builtin_pack` are ordinary options of
the ast-grep engine, and the per-rule overrides ADR 0029 already put under the engine's own table
now share it. The two-tables-named-`rules` ambiguity disappears.

**typos is opt-in.** `[engines.typos] enabled = true` turns it on; the default changes to off.
typos ships as its own component, and a default-on engine would force a download on every
first run. `poly migrate` writes `enabled = true` into any v1 config that relied on the old
default, so existing projects keep their behaviour.

**Project checks use engine tables too.** `[engines.python-types] backend = "ty"` (or
`"pyrefly"`) selects the Python type checker. Whole-project engines are configured exactly like
per-file ones.

### Components, version, root, workspace

```toml
required_version = ">=2.0, <3"
root = true

[components]
enable = "auto"          # or ["python", "web"]
disable = ["ruby"]
install = "auto"         # "auto" | "never"
on_missing = "fail"      # "fail" | "skip"

[workspace]
run = "auto"
```

- **`[components]`** selects which components a project uses. `enable = "auto"` derives them
  from the files discovered and the `[engines.*]` tables present. `install = "never"` forbids
  downloads. `on_missing` decides what a run does when a needed component is unavailable.
  Configuring an engine whose component is disabled is a config error, not a silent no-op.
- **`required_version`** is a semver requirement on poly. Because components are versioned in
  lock-step with poly, it pins every component too; there is no project-level component lock.
- **Top-level `root = true`** replaces `[workspace] root` (ADR 0018). It is a property of the
  config file, not of workspace behaviour.
- **`[workspace] run = "auto" | "always" | "never"`** replaces `[lint] workspace` (a boolean).
- **`per_file_ignores`** replaces `per-file-ignores` (ADR 0017). Every other key in the file is
  snake_case; this was the exception.

### v1 detection and migration

A config in the v1 shape (any `[lint.<lang>.<tool>]` or `[fmt.<lang>.<tool>]` table, a top-level
`[rules]`, `[workspace] root`, `[lint] workspace`, or `per-file-ignores`) is rejected at load
with exit 2 and a message naming `poly migrate --write`. Detection runs before any other
validation so the user sees one actionable error, not a list of unknown keys.

`poly migrate` rewrites a v1 `poly.toml` with `toml_edit`, so comments and ordering survive:

- `[lint|fmt.<lang>.<e>]` becomes `[engines.<e>]`. Values common to the language tables are
  hoisted; keys that exist only under `[lint]` or only under `[fmt]` go to phase sub-tables;
  language-specific values go to `.lang.<id>`.
- `[rules]` becomes `[engines.astgrep]`.
- Workspace keys move to `[workspace]` and top-level `root`.
- `per-file-ignores` becomes `per_file_ignores`.
- `[engines.typos] enabled = true` is written where typos was on by default.
- `required_version` is added.
- `poly-config.lock` and `poly-hooks.lock` merge into `poly.lock`.
- The `$schema` URL is updated.

### CLI

**Global flags.** `--config`; `--color auto|always|never` (replaces `--no-color`); `--offline`
(or `POLY_OFFLINE=1`); `-q`, `-v`, `--debug`. `--offline` governs components, tree-sitter
grammar fetches, catalog installs, remote `extends` and hook sources. Anything that would need
the network under `--offline` is a skip or an error according to `on_missing`, never an attempt.

**`poly lint [PATHS]`.**

- `--workspace auto|always|never` replaces the `--workspace` / `--no-workspace` pair and
  overrides `[workspace] run`.
- `--set KEY=VALUE` replaces `--force-exclude`, `--include-generated` and `--skip-generated`.
  It is a general override for run-scoped discovery options, so the next option of this kind
  does not need a new flag.
- `--on-missing fail|skip` overrides `[components] on_missing`.
- Unchanged: `--fix`, `--only`, `--skip`, `--include-excluded`, `--fix-generated`,
  `--deny-skips`, `--max-skips`, `--exclude`, `--no-cache`, `-j`, `--format pretty|json|toon`.

**`poly fmt`.** Same flags as `lint` except `--workspace`. `--check` is removed because the
dry run is already the default. `--exit-zero-on-fix` is replaced by `--exit-nonzero-on-fix`,
which makes the non-default exit behaviour the explicit opt-in.

**Missing components.** A run that discovers files whose engine lives in a component that is not
installed installs it (`install = "auto"`) or, otherwise, exits 2 (`on_missing = "fail"`). A
skip happens only under `install = "never"`, `--offline` or `--on-missing skip`, and is counted
as a skip so `--deny-skips` and `--max-skips` apply. poly never falls back to the tree-sitter
tier for that language: the output would differ between machines.

**`poly components`** (alias `component`): `list` (from the compiled index), `status [PATHS]
[--verify]`, `install <name|preset|all>`, `uninstall`, `sync [PATHS]`, `prune [--all]
[--dry-run]`, `import <archive>`. Every subcommand takes `--format` and emits `schema: 1`.
`sync` fetches everything a run would need, including grammars and `install = "auto"` tools;
under `--offline` it only checks and exits 2 if anything is missing.

**`poly lock [--upgrade] [--check]`** manages the single `poly.lock`, which replaces
`poly-config.lock` and `poly-hooks.lock`. `poly config update` and `poly hooks update` are
removed.

**`poly config validate`** checks a config against the compiled component index and never
downloads. `poly doctor` gains a components section. `poly cache size` and `poly migrate
--report` are removed.

**The download rule is enforced by command.** Only a lint or format run that discovers files for
a missing component, a config that lists components to install, and an explicit
`components install` / `components sync` may download. `list`, `status`, `config validate`,
`config show` and `doctor` never do.

### Why "component"

The unit users install is a downloadable bundle of engines. The name is `component`, as in
`rustup component`. The alternatives each collide with something poly already has:

- **toolchain** collides with the native-toolchain tier (ADR 0014) and `toolchains.toml`, where
  it means a host-installed first-party CLI such as `gofmt`.
- **plugin** collides with the agent plugin and marketplace of ADR 0022, which is a different
  artifact with a different trust model.
- **pack** collides with the built-in ast-grep rule pack (ADR 0029) and the tree-sitter
  language pack (tslp).

A component bundles engines, and the engine stays the unit users configure. That is why the
config is engine-first and `[components]` is only a selection and policy table.

### Breaking changes

| Change | Old | New | Justification |
| --- | --- | --- | --- |
| Engine config tables | `[lint.<lang>.<tool>]`, `[fmt.<lang>.<tool>]` | `[engines.<id>]` with phase and language sub-tables | The engine is the unit users configure and the unit a component ships; removes per-language and per-phase duplication |
| ast-grep config | top-level `[rules]` (`dirs`, `builtin`) | `[engines.astgrep]` (`rule_dirs`, `builtin_pack`) | It is an engine option; ends the two-`rules`-tables ambiguity |
| Root marker | `[workspace] root` | top-level `root` | A property of the config file, not of workspace behaviour |
| Workspace phase | `[lint] workspace` (bool) | `[workspace] run = "auto\|always\|never"` | One table, and a tri-state matching the CLI flag |
| Ignore table | `per-file-ignores` | `per_file_ignores` | Every other key is snake_case |
| typos default | on | off; `[engines.typos] enabled = true` | typos is an opt-in component; migrate keeps behaviour for v1 configs |
| v1 configs | accepted | exit 2 with a `poly migrate --write` hint | A mixed or silently reinterpreted config would run the wrong rules |
| Colour flag | `--no-color` | `--color auto\|always\|never` | A three-way choice instead of an on/off switch |
| Workspace flags | `--workspace`, `--no-workspace` | `--workspace auto\|always\|never` | Two flags that could contradict become one |
| Discovery flags | `--force-exclude`, `--include-generated`, `--skip-generated` | `--set KEY=VALUE` | Run-scoped overrides without a flag per option |
| Format dry run | `fmt --check` | removed | The dry run is already the default |
| Autofix exit | `fmt --exit-zero-on-fix` | `fmt --exit-nonzero-on-fix` | The opt-in names the exit behaviour it enables |
| Lock files | `poly-config.lock`, `poly-hooks.lock` | `poly.lock` | One file to review and commit |
| Lock commands | `config update`, `hooks update` | `poly lock [--upgrade] [--check]` | One command for one file |
| Cache size | `poly cache size` | removed | Cache and component state is reported by `components status` and `doctor` |
| Migrate report | `poly migrate --report` | removed | Migration output is the only report; one way to see it |
| Missing engine | not possible (all bundled) | exit 2 (`on_missing = "fail"`) unless skipped explicitly | Output must not differ between machines |

Additive and not breaking: `[components]`, `required_version`, `--offline`, `--on-missing`,
`poly components`, `poly config validate`.

## Consequences

**Positive.** One table per engine, whatever component it ships in. The config validates offline.
The network behaviour of every command is stated in the CLI. A fresh machine either produces
the same output as a configured one or fails with exit 2; it never degrades silently. The
three-way flags (`--color`, `--workspace`) say what they mean, and `--set` absorbs future
discovery overrides without new flags.

**Negative.** Every v1 user must run `poly migrate --write`, and CI that pins `poly fmt --check`
or `--exit-zero-on-fix` must change its command lines. Anything that parses `poly-config.lock`
or `poly-hooks.lock` breaks. A project that relied on typos by default and does not migrate
loses spell checking silently once its config is rewritten by hand; migrate covers the
mechanical path only. The documented `[lint.<lang>.<tool>]` shape appears throughout earlier
ADRs, which now describe the v1 schema; they are amended with a link here and keep their
original text.

**Risk.** The migration is lossy where v1 expressed per-language values that differ only by
phase; those become language sub-tables and the result is listed in migrate's output. Hoisting
values into `[engines.<id>]` must not change any engine's resolved config, so migrate is tested
by resolving the v1 and v2 configs and comparing the result.

### Confirmation

- A v1 detection test in `poly-config` covers each v1 marker (engine tables, `[rules]`,
  `[workspace] root`, `[lint] workspace`, `per-file-ignores`) and asserts exit 2 with the migrate
  hint; a valid v2 config loads.
- Migrate tests run each fixture through `poly migrate`, assert comments survive, and assert the
  v1 config resolved the old way equals the migrated config resolved the new way. A typos fixture
  asserts `enabled = true` is written only where it was on by default.
- CLI tests assert the removed flags are rejected, the new flags parse, `--workspace` and
  `--color` accept only their values, and `fmt --exit-nonzero-on-fix` sets the exit code.
- Offline tests with nothing installed assert exit 2 for a lint of a covered file, a counted skip
  under `--on-missing skip`, and that `components status`, `config validate`, `config show` and
  `doctor` make no network request.
- The published JSON Schema is regenerated from the v2 types and checked for drift.

## Alternatives considered

- **Keep `[lint|fmt.<lang>.<tool>]` and add a component mapping.** Users would configure an
  engine under up to eight paths and also need to know its component. Rejected.
- **Accept v1 configs with a deprecation warning.** Two schemas to resolve and test, and every
  warning would be ignored until the removal release. Rejected: one clean break, with a
  mechanical migration.
- **Auto-migrate in memory at load time.** Hides the change, and a silently rewritten config can
  differ from what the user reads. Rejected; `poly migrate` writes the result.
- **A project-level component lock.** Components version in lock-step with poly and
  `required_version` pins poly, so a second lock would only be able to disagree with it.
- **Keep typos default-on and let its component auto-install.** Makes every first run download an
  optional tool. Rejected; migrate preserves the old behaviour for existing configs.
- **Fall back to the tree-sitter tier when a component is missing.** Output would depend on what
  is installed on the machine. Rejected.
- **The nouns toolchain, plugin and pack.** Rejected; see "Why component".
