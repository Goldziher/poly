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
Content-Hash: blake3:a7581f394fd984a61613639eac329a827d981dfecd4d88ec9ff3b9cc3bc98447
Source-Hash: blake3:f9174ffba9b3be98e4322fd06197faffec11238e87265c535f5a4a06c89006c1
Schema-Version: v1
-->

# poly as the Orchestrator

Treat poly as the **single** lint and format gate for the repository. Do not invoke ruff,
oxlint, taplo, rumdl, or the rest directly — poly compiles them in as backends, and covers
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
  for tools ruff replaces. It deletes a source only when nothing in it was lost.
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
- **`poly config show`** prints the effective merged config; **`poly config update`** pins a
  symbolic remote `extends` ref into `poly-config.lock`; **`poly config schema`** prints the
  JSON Schema for `poly.toml`. An unknown key in an engine table is an `unknown-config-key`
  warning that suggests the nearest valid key (`--verbose` lists every key the table reads).
- **`poly doctor`** reports which poly is running, every poly on PATH, and the config in
  effect — the first thing to run when two machines disagree.

## CI

`Goldziher/poly@v0` is a **setup** action: it installs the `poly` binary (with optional
caching) and puts it on `PATH`. It does not run poly for you — invoke the commands yourself
in following steps.

```yaml
- uses: actions/checkout@v4
- uses: Goldziher/poly@v0        # inputs: version (default latest), cache, github-token
- run: poly fmt --check .
- run: poly lint .
```

poly is one pinned binary with no *required* system dependencies, so local, hook, and CI
runs agree — with one caveat: the native-toolchain tier means `rustfmt`, `gofmt` and `shfmt`
(and `shellcheck` for lint) are used automatically whenever they are on `PATH`, and output then
depends on that tool's version. A machine without them silently falls through to the
lower-fidelity tree-sitter tier. Pin those toolchains in CI (and expect drift otherwise), or set
`[fmt.rust.rustfmt] enabled = false` / `[fmt.go.gofmt] enabled = false` /
`[fmt.shell.shfmt] enabled = false` to stop the subprocess. That does not leave the language
unformatted: native-toolchain backends read `enabled` themselves and hand the file to the
tier-2 reindenter when disabled, rather than expecting the runner to drop them from the plan —
so the file is still formatted, just at lower fidelity than the native tool.
