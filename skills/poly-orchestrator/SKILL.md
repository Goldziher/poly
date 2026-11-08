---
description: Use poly as the single lint/format gate instead of invoking ruff/oxlint/rustfmt directly — one poly.toml, poly hooks install, poly migrate, CI via the Goldziher/poly setup Action
name: poly-orchestrator
license: MIT
metadata:
    author: Goldziher
    repository: https://github.com/Goldziher/poly
---

<!--
AI-RULEZ :: GENERATED FILE — DO NOT EDIT
Content-Hash: blake3:a397e480d18dad14dc69c7d63f97ac72fcd8e9f1020c6308276d4ae7e6b11f2b
Source-Hash: blake3:d423fda6aef47ec0661e9460a04f849c62839d8306d1a6e168cc1314db55f78b
Schema-Version: v1
-->

# poly as the Orchestrator

Treat poly as the **single** lint and format gate for the repository. Do not invoke ruff,
oxlint, taplo, rumdl, or the rest directly — poly runs them as engines (linked in, or downloaded
as signed components), uses host tools such as `rustfmt` and `gofmt` when installed, and covers
everything else via the tree-sitter generic tier, the `quality` metric tier, and its built-in
ast-grep rule pack, behind one binary, one config, and one report.

## Adopt it

- **One `poly.toml`.** Configure every language and rule in a single per-repo file
  (`poly.local.toml` for local overrides, `extends` to share a base). Layering is
  tool default → poly's opinionated overrides → your `poly.toml`. In a monorepo a nested
  `poly.toml` deep-merges on top of its ancestors for the files beneath it.
- **`poly migrate`** — absorb existing tool configs into `poly.toml` instead of
  hand-writing it. It imports **ruff, typos, taplo, and markdownlint** configs, and turns a
  **lefthook** or **pre-commit** config into `[hooks]`, `[hooks.builtin]` and `[tools]`
  entries; there is no eslint or prettier importer (an existing `.oxlintrc.json` /
  `.oxfmtrc.json` is read directly, so it needs none). The default is a dry-run report — pass
  `--write` to apply (`--allow-dirty` on a dirty tree), `--recurse` for a monorepo, `--verify`
  to re-run poly afterwards, and `--strip-superseded` to also remove `pyproject.toml` sections
  for tools ruff replaces. It deletes a source only when nothing in it was lost. `--to-v2`
  rewrites an existing v1 `poly.toml` (`[lint.<lang>.<tool>]`, `[rules]`, `per-file-ignores`, …)
  into the v2 schema (`[engines.<id>]`, `[workspace] run`, `per_file_ignores`), keeping comments.
- **`poly hooks install`** — wire the git-hook shims so `poly` runs the configured stages on
  every commit, replacing a `.pre-commit-config.yaml` or lefthook setup. It installs the hook
  types your `poly.toml` configures; `--hook-type` picks specific ones. A foreign hook already
  there is kept as `<hook>.legacy` and run before poly; a lefthook hook is kept but not run
  (`--replace-lefthook` discards it), and `--overwrite` discards any preserved hook.
  `poly hooks uninstall` restores what was there before; `poly hooks run [stage]` runs a stage
  by hand (`--all-files` for the whole tree, `-v` to show passing jobs' output).
- **External tools** — enable any catalog tool with `[tools.<name>] enabled = true`
  (`golangci-lint`, `actionlint`, `hadolint`, `helm`, `tflint`, `buf`, …). Pin `version` and set
  `install = "auto"` to have poly download a checksum-verified release when it is missing; by
  default it never downloads anything.
- **`poly config show`** prints the effective merged config; **`poly lock [--upgrade] [--check]`**
  pins symbolic remote `extends` refs and Git hook sources into `poly.lock`; **`poly config schema`** prints the
  JSON Schema for `poly.toml`. An unknown key in an engine table is an `unknown-config-key`
  warning that suggests the nearest valid key (`--verbose` lists every key the table reads).
- **Components.** Only the Linux musl release binary is bundled (every engine linked in). Every
  other release binary is a core build that downloads engines as signed components on first use
  (`poly rules` installs the `astgrep` component the same way): `poly components
  list|status|sync|install`, and `--offline` / `POLY_OFFLINE=1` forbids downloads. A needed
  component that cannot be installed is exit `2`, or a counted skip under `on_missing = "skip"`.
  `[components]` in `poly.toml` sets `install` and `on_missing`; `[engines.typos] enabled = true`
  opts into spell-checking.
- **`poly doctor`** reports which poly is running, every poly on PATH, and the config in
  effect — the first thing to run when two machines disagree.

## CI

`Goldziher/poly@v0` is a **setup** action: it installs the `poly` binary (with optional
caching) and puts it on `PATH`. It does not run poly for you — invoke the commands yourself
in following steps.

```yaml
- uses: actions/checkout@v4
- uses: Goldziher/poly@v0        # inputs: version (latest, or from-config), config-file, components, cache, github-token
- run: poly fmt .
- run: poly lint --deny-skips .
```

`--deny-skips` makes a file poly could not check an exit-2 failure, so CI cannot pass while
covering nothing.

Engines and components are pinned to the poly version, so local, hook, and CI runs agree —
with one caveat: `rustfmt` and `gofmt` (and `shellcheck` for lint) are host tools used
automatically whenever they are on `PATH`, and output then depends on that tool's version. A
machine without them falls through to the lower-fidelity tree-sitter tier with a notice. Pin
those toolchains in CI (and expect drift otherwise), or set `[engines.rustfmt] enabled = false` /
`[engines.gofmt] enabled = false` to stop the subprocess. That does not leave the language
unformatted: native-toolchain backends read `enabled` themselves and hand the file to the
tier-2 reindenter when disabled — so the file is still formatted, just at lower fidelity.

Shell is different. It is formatted by the `sh` component, not a host tool, and
`[engines.shfmt] enabled = false` leaves shell **unformatted**; it does not fall back to the
tree-sitter reindenter.
