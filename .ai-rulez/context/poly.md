---
priority: high
---

# poly

poly is a single-binary, multi-language linter and formatter. It bundles engines (ruff, oxc, taplo, rumdl) and delegates to native tools (cargo fmt/clippy, golangci-lint, actionlint, shellcheck, shfmt) when present.

## Commands

- Lint: `poly lint .`
- Lint, per-file tier only (skip whole-project tools): `poly lint --no-workspace .`
- Check formatting (dry-run): `poly fmt --check .`
- Apply formatting only (no linting): `poly fmt --fix .`
- Apply lint autofixes (and whole-project autofixes): `poly lint --fix .`

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

**Plain `poly lint` is therefore not read-only.** poly applies no fixes without `--fix` — the
per-file tier only writes under `--fix`, and the phase is asked for check mode — but the phase
*executes* the configured tools against the live worktree, and their own side effects are not
poly's to control (`cargo clippy` populates `target/` and can refresh `Cargo.lock`; a `go`
invocation can append to `go.work.sum`; a type checker writes its cache). A run that must leave
the tree untouched needs `--no-workspace` or `[lint] workspace = false`.

The phase is also **not path-scoped**: `poly lint <paths>` skips it for that reason, and
`--workspace` opts back in with the tools still covering the whole repository — regardless of the
named paths and of `[discovery] exclude`, which filters poly's own discovery only. poly prints a
note on both branches.

Under `--fix`, this phase runs the tools in **fix mode**: `cargo sort` sorts in place,
`cargo-machete --fix` prunes unused deps, and `cargo clippy --fix --allow-dirty --allow-staged`
applies clippy autofixes (`cargo deny` has no autofix and stays check-only). Fix mode is what
`--fix` adds; the phase itself runs either way, so `--no-workspace` — not the absence of `--fix` —
is what skips it. `poly fmt` is a pure formatter — it never runs the whole-project phase, since
that phase is linting, not formatting. The git-hook / commit-gate path never requests fix mode.

The orchestration itself lives in `crates/poly-workspace` (`run_workspace_lint`), shared by
`poly-cli` and the `poly mcp` `workspace_lint`/`workspace_lint_fix` tools so both surfaces run
identical behavior against the live worktree.

## Agent distribution

poly publishes its own Claude/Codex plugin (`Goldziher/poly` marketplace) registering `poly mcp`
as a stdio server plus 5 skills and 2 slash commands — generated from `.ai-rulez/` into
`.claude-plugin/`/`.codex-plugin/` (never hand-edit the generated files). Install with
`/plugin marketplace add Goldziher/poly` then `/plugin install poly@poly`. The plugin assumes
`poly` is already on `PATH`; it does not bundle a binary. Bump the lock-step version with
`scripts/release-bump.sh <version>`.

## Configuration

Per-repo `poly.toml` (with `poly.local.toml` for local overrides). A top-level `extends` list
shares any config section from local or pinned-remote base configs (ADR 0020) — bases merge
beneath the file, `poly.local.toml` wins on top; `poly config update` locks a symbolic remote
ref into `poly-config.lock`, and `poly config show` prints the effective merged config. The
result cache and hook staged snapshot live in the per-user OS cache dir (`~/.cache/poly/<repo-key>`
on Linux, `~/Library/Caches/poly/…` on macOS, `%LOCALAPPDATA%\poly\…` on Windows) — not in-repo.
`POLY_CACHE_HOME` overrides the base; `[cache] dir` pins an explicit path.

## Severity

`poly lint` exits non-zero only on error-severity findings; warnings don't fail CI.

## CI

Validation runs via `uses: xberg-io/actions/.github/workflows/reusable-validate.yml@v1`.

Run `poly fmt --check .` and `poly lint .` after changes to verify compliance.
