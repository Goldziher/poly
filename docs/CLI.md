# CLI reference

Full flag reference for `poly lint` / `poly fmt`, plus `poly doctor`, `poly config`,
`poly commit`, `poly hooks`, `poly cache`, `poly mcp`, `poly migrate`, and `poly rules`. See the
README's [CLI reference](../README.md#cli-reference) section for a quickstart.

## lint and format

```text
poly lint [PATHS]...
poly fmt [PATHS]...

  --fix                        Apply lint fixes or formatting in place.
  --fix-generated              Also rewrite files whose header stamps a content hash over the
                               body (`<project>:hash:<digest>`). Those are the only generated
                               files poly withholds a write from — `poly fmt` skips them and
                               `poly lint --fix` reports without rewriting — because
                               reformatting invalidates the hash and the generator's verify
                               step then reports drift on a file nobody edited. A plain
                               `DO NOT EDIT` / `@generated` banner does not hold poly back:
                               those files are linted, formatted and fixed like any other, so
                               this flag has no effect on them.
  --skip-generated             Do not lint or format machine-generated files at all (banner or
                               stamp). Overrides `[discovery] generated` for this run; the
                               files are reported as skipped, so they count against
                               --deny-skips / --max-skips. Conflicts with --include-generated.
  --include-generated          Lint and format machine-generated files. This is the default,
                               so it only matters in a repo that set
                               `[discovery] generated = false`. Conflicts with --skip-generated.
  --check                      `poly fmt` only. Explicit dry run. This is the default.
  --workspace                  `poly lint` only. Run the whole-project phase even though
                               explicit paths were given (normally a path-scoped run skips it).
                               The phase is never path-scoped: the tools cover the whole
                               repository regardless of the named paths and of `[discovery]
                               exclude`, and the run says so on stderr. Conflicts with
                               --no-workspace.
  --no-workspace               `poly lint` only. Skip the whole-project phase (cargo
                               clippy/-sort/-machete/-deny and any other configured
                               whole-workspace tools). Equivalent to `[lint] workspace = false`.
                               Also what makes a run read-only: without it, plain `poly lint`
                               executes those tools against the live worktree, and their own
                               side effects are not poly's to control.
  --format <pretty|json|toon>  Output format. Default: pretty.
  --config <PATH>              Use an explicit config file.
  --exclude <GLOB>             Exclude paths from discovery (repeatable; merged
                               with `[discovery] exclude`). An unanchored glob
                               matches at any depth; lead with `/` to anchor it
                               to the config directory.
  --force-exclude              Apply `[discovery] exclude` to explicitly named files too.
                               This is the default, so it only matters in a repo that
                               set `[discovery] force_exclude = false`.
  --include-excluded           Check explicitly named files or directory roots even when excluded.
                               Overrides `[discovery] force_exclude`. Exclusions below an
                               included directory remain active.
  --deny-skips                 Exit 2 if any file was skipped. Equivalent to
                               `--max-skips 0`.
  --max-skips <N>               Exit 2 if more than N files were skipped.
  --no-cache                   Bypass the result cache.
  -j, --jobs <N>               Parallel jobs. Default: logical cores.
  --no-color                   Disable colored output.
  -q, --quiet                  Trim pretty output to the findings and the summary: drop the
                               per-file discovery and skip detail printed beneath it. Every
                               count and reason stays in the summary — how many files were
                               linted, how many were skipped and why — so nothing is hidden,
                               only un-itemised. Findings still print and exit codes are
                               unchanged. No effect on --format json/toon. Conflicts with
                               --verbose and --debug.
  --verbose                    Pretty output includes descriptions, URLs, and metadata,
                               and names every skipped file rather than the first 3 per
                               skip reason. Conflicts with --quiet.
  --debug                      Include cache hit/miss and timing data. Implies --verbose:
                               the levels are a ladder (quiet < normal < verbose < debug),
                               each a superset of the one below. Conflicts with --quiet.
```

Skipped files — a language poly has no lint rules for, no matching engine, a generated
file, an unreadable path — are always counted and their reasons summarized, so a run that
checked nothing cannot look like a clean pass. `--deny-skips` / `--max-skips` turn that
into a hard failure for CI.

`poly lint` reports a file whose language nothing in the run lints as
`skipped main.zig: no lint rules for Zig`, and keeps it out of the linted count. This covers
the tier-2 languages the code-quality tier cannot model — Zig, Swift, Dart, Gleam, Elixir,
Nix, Scala, Lua, R — and any language whose linter is opt-in or missing from `PATH` — a shell
script with no `shellcheck` installed is reported rather than counted. It is coverage
information, not a failure: the run still exits 0 unless you ask for `--deny-skips` /
`--max-skips`. Cross-cutting checks (typos, ast-grep rules, comment removal) still run on
these files and their findings are still reported.

A file a directory walk could not identify as any language is counted separately —
`N file(s) of unrecognized type not checked`, with the first few named — rather than
itemised as a skip, since every repository is full of images, lock files and snapshots that
no linter was ever going to read. A path you name on the command line is different: naming
it is a request to check it, so it is reported as a skip.

**Exit codes:**

| Code | Meaning |
|---:|---|
| 0 | Clean: no lint findings, no formatting drift, or all writes succeeded. |
| 1 | Error-severity lint findings remain, a whole-project tool (`cargo clippy`, …) failed, or (for `poly fmt`) files would change. Warning-severity findings alone never trigger this — typos and the quality tier don't redden CI. |
| 2 | The run verified less than it claims: a file an engine failed on, a skip budget exceeded (`--deny-skips` / `--max-skips`), a config/pipeline error, or a machine-readable report that failed to serialize. |

A `--format json`/`toon` consumer must check the exit code, not just the payload — the
whole-project phase's own pass/fail is written to stderr, not the stdout document.

## doctor — which poly am I actually running?

```sh
poly doctor                  # human report; exits 1 when something is actively wrong
poly doctor --format json    # the same report, for a bug report or a CI check
```

Run this before filing a bug. It prints the resolved path of the running executable with its
version and **build identifier**, every `poly` on `PATH` in order with the version each one
reports, the config files in effect, and the cache directory — then exits non-zero on a real
defect: a competing install on `PATH`, a `poly` that cannot report its own version, or a config
that fails to load. Each finding carries the concrete remedy, including the fact that a
cargo-installed `~/.cargo/bin/poly` needs `rm`, not `cargo uninstall poly`.

`poly --version` reports the build identifier too — `0.23.1 (release build v0.23.1, release)`
versus `0.23.1 (dev build v0.23.1-8-g18aa5e8, debug)` — so a development build carrying
unreleased changes cannot be quoted as a release. The identifier comes from `git describe` at
build time; outside a git checkout it reads `unknown` rather than guessing (packagers can set
`POLY_BUILD_ID`).

When another `poly` on `PATH` differs from the running one, every command warns once on stderr
and points at `poly doctor`. A correctly-installed poly finds a single entry and prints nothing;
`POLY_NO_SHADOW_WARN=1` silences it regardless.

## config — what did poly actually parse?

```text
poly config show [--config <PATH>] [--format <toml|json|toon>]
poly config update [--config <PATH>]
```

`poly config show` prints the **effective, fully-merged configuration** — every section and
key poly resolved after `extends` bases, the nested `poly.toml` cascade and `poly.local.toml`
have all been applied:

```toml
# poly effective configuration
#
# config:      /repo/poly.toml
# merged from: /repo/poly.toml
#              /repo/poly.local.toml
# extends:     path ../baseline/poly.toml
# hooks:       present

[defaults]
line_length = 120
...

[lint.python.ruff]
mccabe_max_complexity = 3
```

The default output is a valid TOML document, so `diff` against your own `poly.toml` shows
exactly what poly kept — including keys poly does not recognize, which are printed as written
rather than dropped. `[defaults]`, `[discovery]`, `[rules]` and `[workspace]` are shown fully
resolved, so a setting nobody wrote (`line_length = 120`) is still visible; every other section
is shown exactly as merged.

`--format json` / `--format toon` emit the same document as
`{ "config": …, "resolution": … }`, where `resolution` carries the config path, the files that
were merged, the resolved `extends` bases, and whether `[hooks]` is present — the facts the TOML
form carries as comments. The `merged from` list is file-level attribution: it names the files
that could have contributed a value, not which file each key came from.

`poly config update` resolves symbolic `extends` git refs to pinned object IDs and writes
`poly-config.lock`. Config validation reports unknown top-level sections, unknown keys within a
recognized section, and wrongly-typed values as warnings — a misspelled key no longer parses
silently and does nothing.

## commit, hooks, cache, and MCP

```sh
poly commit --message "feat: add backend"   # or a path: poly commit .git/COMMIT_EDITMSG
poly hooks install
poly cache stats
poly cache size
poly cache gc
poly cache clean
poly mcp --config /path/to/poly.toml
poly doctor                # which poly is running, what's on PATH, config + cache
poly migrate               # dry-run: report what would move into poly.toml
poly migrate --write       # absorb tool configs into poly.toml, remove redundant files
```

`poly migrate` folds settings from `ruff`/`taplo`/markdownlint/`typos` config files
(and `pyproject.toml` `[tool.ruff]`/`[tool.typos]`/`[tool.codespell]`) into `poly.toml`,
then deletes or strips only the sources poly can fully honor — files it delegates to
(`rustfmt.toml`, `.golangci.yml`, `clippy.toml`, …) and anything not fully representable
are kept. It is a dry-run report by default (`--report` says so explicitly); `--write` applies,
`--recurse` walks nested projects, `--verify` re-runs lint/format after writing,
`--strip-superseded` additionally strips `pyproject.toml` `[tool.*]` sections for the Python
tools ruff supersedes (black, isort, flake8, …), and `--allow-dirty` lets it write into a
repository with uncommitted changes. It takes an optional
path, defaulting to the current directory.

See [poly mcp](../.ai-rulez/skills/poly-mcp/SKILL.md) for the full MCP tool surface, or the
README's [AI Agents & MCP](../README.md#ai-agents--mcp) section for a summary. In short: the
server is **stdio-only**; `lint`, `format_check`, `cache_stats`, `rules`, `config_show`, and
`version` are read-only; `lint_fix`, `format_write`, and `cache_clean` are mutating;
`workspace_lint` and `workspace_lint_fix` run the whole-project phase as async **Tasks** (the
call returns a task handle and the client polls `tasks/get`; a client without the tasks
capability gets a synchronous result instead).

## custom rules

```sh
poly rules test [DIR]...    # verify rules against their *-test.yml snippets
poly rules list [DIR]...    # list every resolved rule (built-in pack + user rules)
poly rules list --format json   # same rows as JSON (also: --format toon)
```

`poly rules list` prints one row per rule — id, language, `builtin`/`user`, the severity it
reports at under the current config, and the rule's own declared default (`off` for an opt-in
rule) — so a warning you did not recognise can be traced to the rule that raised it and turned
off. `--format json` / `--format toon` carry the same fields.

With no `DIR`, both read `[rules] dirs` from the nearest `poly.toml`. `poly rules test` exits
non-zero on any failed snippet (a `valid` snippet that matched, an `invalid` one that didn't, a
`fixed:` autofix that differed, or a test naming an unknown rule id).
