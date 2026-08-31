---
priority: high
---

# poly

poly is a single-binary, multi-language linter and formatter. Tier-1 backends are compiled in as
crate dependencies — ruff, oxc, biome (CSS/GraphQL), mago (PHP), taplo, rumdl, sqruff, malva,
markup_fmt, rubyfmt, nixfmt, typos, and more under `crates/poly-core/src/engines/` — with a
tree-sitter generic tier for the long tail. Two cross-cutting lint tiers run on top: the native
`quality` metric engine and ast-grep with a built-in rule pack. Canonical first-party CLIs are
used when found on `PATH` (`rustfmt`, `gofmt` and `shellcheck` are default-on; `zig fmt`,
`shfmt`, `google-java-format`, `ktfmt`, `swift-format`, `dartfmt`, `styler`, `gleamfmt` are
opt-in), and
whole-project tools (`cargo clippy` / `cargo-sort` / `cargo-machete` / `cargo-deny`, plus opt-in
catalog tools such as `golangci-lint` and `actionlint`) run in the whole-project phase, not the
per-file tier.

## Commands

- Lint: `poly lint .`
- Lint, per-file tier only (skip whole-project tools): `poly lint --no-workspace .`
- Check formatting (dry-run, the default): `poly fmt --check .`
- Apply formatting only (no linting): `poly fmt --fix .`
- Apply lint autofixes (and whole-project autofixes): `poly lint --fix .`

Both `lint` and `fmt` share: `--format pretty|json|toon`, `--config <PATH>`, `--no-cache`,
`-j/--jobs <N>`, `--exclude <GLOB>` (repeatable), `--no-color`, `--fix`, `--verbose` /
`-q/--quiet`, `--debug`, `--force-exclude` / `--include-excluded`, `--fix-generated`,
`--skip-generated` / `--include-generated`, `--only <ENGINE>` / `--skip <ENGINE>` (restrict the
per-file tier to named engines, comma-separated and repeatable — narrows only, never enables a
backend config left off, and skips the whole-project phase since it is tools, not engines), and
`--deny-skips` / `--max-skips <N>`. `poly lint` adds `--no-workspace` and `--workspace`.

The other subcommands: `poly hooks` (native git-hook runner over `[hooks]`), `poly commit`
(commit-message lint, gitfluff), `poly rules test|list` (custom + built-in ast-grep rule packs),
`poly config update|show`, `poly cache`, `poly migrate`, `poly mcp`, `poly doctor`.

## Machine-readable output

`--format json`/`toon` renders an object, not a bare array: `{results, errors, skipped, summary,
configs}`. `summary` is `{checked, skipped, errored}` — the run's own account of what it did,
and the field to gate on. The counts do not sum to `results.len()`: a file that was checked and
found clean produces no `results` entry at all, and a file can appear in both `results` (as a
synthetic entry with empty diagnostics) and in the top-level `skipped`/`errors` arrays — those
top-level arrays are redundant with `results` on purpose, so a consumer scanning either one
cannot miss what the other names. `configs` holds one `ConfigFingerprint` per directory-scoped
config that governed the run, indexed by each result's `config` field.

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
note on both branches. Naming the workspace root is *not* path scoping: `poly lint .` is how
people say "lint everything", so it runs the phase.

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
`POLY_CACHE_HOME` overrides the base; `[cache] dir` pins an explicit path. `[hooks]
snapshot_include` symlinks named, git-untracked paths into that staged snapshot for a
`workspace` hook whose build needs to read a gitignored input; an included file's fixes are
withheld and it is never a per-file hook's own input.

## Code-quality tier and rule packs

Beyond the per-language backends, two cross-cutting lint mechanisms run on by default at
**warning** severity (ADR 0027), so adopting them does not redden CI:

- **The native `quality` engine** — `file-too-long` (1000 lines), `function-too-long` (80),
  `type-too-long` (300), `too-many-parameters` (6), `nesting-too-deep` (4),
  `cyclomatic-complexity` (20) and `lazy-ignore`, plus opt-in `magic-number` and `law-of-demeter`.
  Configured under `[lint.quality]` / `[lint.<lang>.quality]` with flat keys — a boolean toggle
  plus a threshold, e.g. `file_too_long = false`, `file_too_long_lines = 800`,
  `cyclomatic_complexity_max = 15`; `enabled = false` disables the tier. It never duplicates a
  tier-1 backend: a per-language deferral table yields each metric to the existing rule (ruff
  `C901`/`PLR0913`, oxlint `max-depth`/`max-params`). It claims lint *coverage* only for
  languages whose control flow it
  can model — Python, Rust, Go, JavaScript, TypeScript, TSX, Java, Kotlin, C, C++, C#, Ruby —
  while others still get the line-counting floor without being counted as linted.
- **The built-in ast-grep rule pack** (ADR 0029) — 26 rules across C#, Elixir, Go, Java, Kotlin,
  Python, Ruby, Rust and Swift, embedded in the binary and loaded through the same parse path as
  user rules. It sits *beneath* `[rules] dirs` (a user rule with the same `id` replaces a pack
  rule). Each rule carries its own `severity:`, 13 of the 26 `off`. `[rules] builtin = false`
  disables the pack wholesale; `[lint.astgrep]` `select`/`extend_select`/`ignore` and
  `[lint.astgrep.rules.<id>] level` move individual rules. (`[rules]` — `dirs`, default
  `[".poly/rules"]`, and `builtin` — is a separate top-level table from the per-rule overrides
  that live under a backend's own `[lint.…]` table.)

## Inline suppression

`poly: allow[RULE] reason` and `poly: allow-file[RULE] reason` (ADR 0028), written in the host
language's own comment syntax — detected by a heuristic over known comment openers, not a parse.
An `allow` on a comment-only line covers the next non-blank line; trailing after code it covers
that line; `allow-file` covers the whole file, and is the only form that can suppress a
span-less diagnostic. `RULE` may be a list, or `*` for every rule. **A reason is mandatory**: a
directive with no alphanumeric text after `]` does not suppress at all and reports `lazy-ignore`
instead. The filter is applied centrally in the runner, so every engine inherits it. The file-glob
mechanism `[per-file-ignores]` (ADR 0017) still exists for whole-file exemptions.

## Exit codes

- `0` — clean.
- `1` — **error-severity** lint findings, a failing whole-project tool, or (for `poly fmt`)
  files that would change. Warning-severity findings alone do not fail a run, so `typos` and the
  quality tier don't redden CI.
- `2` — the run verified less than it claims: a file poly failed on — including a `poly fmt`
  file that did not converge to a fixed point within its five-pass cap, reported as an error
  rather than as formatted, since a following `poly fmt --check` would keep reporting drift on
  it — a skip budget exceeded (`--deny-skips` / `--max-skips`), a config/pipeline error, or a
  machine-readable report that failed to serialize.

A `--format json`/`toon` consumer must check the exit code, not just the payload — and gate on
`summary.checked`, not on `results.len()`.

## CI

`.github/workflows/ci.yaml` runs five jobs on push/PR to `main`: `cargo fmt --all --check`;
`cargo clippy --workspace --exclude conformance --all-targets -- -D warnings` and
`cargo test --workspace --exclude conformance --no-fail-fast`, both on a Linux/macOS/Windows
matrix; `dogfood`, which runs the binary against poly's own repo (`poly lint --no-workspace .`
then `poly fmt --check .`) on Linux and Windows; and `cargo-deny check`.
`.github/workflows/publish.yaml` builds and uploads the release artifacts.

The hardening harness (`task harden`, `scripts/harden.sh`) is **not** a CI job. It clones large
third-party trees and its per-rule counts inform a decision rather than pass or fail a commit, so
it is run deliberately — before a release, or when a rule's severity is in question.

Run `poly fmt --check .` and `poly lint .` after changes to verify compliance.
