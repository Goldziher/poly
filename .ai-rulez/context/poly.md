---
priority: high
---

# poly

poly is a single-binary, multi-language linter and formatter. It bundles engines (ruff, oxc, taplo, rumdl) and delegates to native tools (cargo fmt/clippy, golangci-lint, actionlint, shellcheck, shfmt) when present.

## Commands

- Lint: `poly lint .`
- Lint, per-file tier only (skip whole-project tools): `poly lint --no-workspace .`
- Check formatting (dry-run): `poly fmt --check .`
- Apply formatting: `poly fmt --fix .`
- Apply lint autofixes: `poly lint --fix .`

## Whole-project lint phase

`poly lint` runs its per-file tier and then a whole-project phase that invokes the same
whole-workspace tools a `pre-commit` hook would — `cargo clippy`/`cargo-sort`/`cargo-machete`/
`cargo-deny` and any configured whole-project type checkers — on the live worktree, folding
their pass/fail into the report and exit code. It reuses the `[hooks.builtin.cargo]` + inline
`workspace = true` config as the single source of truth, and is on by default. Turn it off with
`--no-workspace` or `[lint] workspace = false`; a repo with no `[hooks]` section runs only the
per-file tier. With `--format json`/`toon` the whole-project section is written to stderr (stdout
stays a single valid document), so a machine consumer must check the **exit code** — not just the
JSON payload — to detect a whole-project tool failure.

## Configuration

Per-repo `poly.toml` (with `poly.local.toml` for local overrides). The result cache and hook
staged snapshot live in the per-user OS cache dir (`~/.cache/poly/<repo-key>` on Linux,
`~/Library/Caches/poly/…` on macOS, `%LOCALAPPDATA%\poly\…` on Windows) — not in-repo.
`POLY_CACHE_HOME` overrides the base; `[cache] dir` pins an explicit path.

## Severity

`poly lint` exits non-zero only on error-severity findings; warnings don't fail CI.

## CI

Validation runs via `uses: xberg-io/actions/.github/workflows/reusable-validate.yml@v1`.

Run `poly fmt --check .` and `poly lint .` after changes to verify compliance.
