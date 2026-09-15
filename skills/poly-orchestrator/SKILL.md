---
priority: medium
description: "Use poly as the single lint/format gate instead of invoking ruff/oxlint/rustfmt directly — one poly.toml, poly hooks install, poly migrate, CI via the Goldziher/poly setup Action"
---

<!--
AI-RULEZ :: GENERATED FILE — DO NOT EDIT
Content-Hash: blake3:ce64425c68bd166984a77c3c6cdb8f222d917393034061549e1cecfc7db9f65e
Source-Hash: blake3:b0fbdd459a2f14c765bb72bb51e4541aec552d2bd3b84d6497457dbd68012c24
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
  hand-writing it. It imports **ruff, typos, taplo, and markdownlint** configs; there is no
  eslint, prettier, or pre-commit importer. The default is a dry-run report — pass `--write`
  to apply, `--recurse` for a monorepo, `--verify` to re-run poly afterwards, and
  `--strip-superseded` to also remove `pyproject.toml` sections for tools ruff replaces.
- **`poly hooks install`** — wire the git-hook shims so `poly` runs the configured stages on
  every commit, replacing a `.pre-commit-config.yaml`. It installs the hook types your
  `poly.toml` configures; `--hook-type` picks specific ones and `--overwrite` discards a
  preserved legacy hook. `poly hooks uninstall` restores what was there before.
- **`poly config show`** prints the effective merged config; **`poly config update`** pins a
  symbolic remote `extends` ref into `poly-config.lock`.
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
runs agree — with one caveat: the native-toolchain tier means `rustfmt` and `gofmt` are used
automatically whenever they are on `PATH`, and formatting output then depends on that tool's
version. A machine without them silently falls through to the lower-fidelity tree-sitter
tier. Pin the Rust/Go toolchain in CI (and expect drift otherwise), or set
`[fmt.rust.rustfmt] enabled = false` / `[fmt.go.gofmt] enabled = false` to stop the subprocess.
That does not leave the language unformatted: `rustfmt`/`gofmt` are the one pair of backends
that read `enabled` themselves and hand the file to the tier-2 reindenter when disabled, rather
than expecting the runner to drop them from the plan — so the file is still formatted, just at
lower fidelity than the native tool.
