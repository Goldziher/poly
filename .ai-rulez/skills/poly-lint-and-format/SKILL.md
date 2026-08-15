---
priority: medium
description: "Running poly lint / poly fmt — --fix, --format json|toon, --exclude, --config, exit codes, and the check → read-json → fix → re-check loop"
---

# poly Lint and Format

## Commands

- `poly lint [PATHS]…` — run the linters. `--fix` applies autofixes (and the whole-project
  fix phase). `--no-workspace` restricts to the per-file tier — and is also what makes a run
  read-only: without it, plain `poly lint` still *executes* the configured whole-project tools
  against the live worktree, and their own side effects (a refreshed lock file, a populated
  build or type-checker cache) are not poly's to control. Naming `[PATHS]…` skips that phase by
  default; `--workspace` opts back in, and the tools then cover the whole repository regardless
  of the named paths and of `[discovery] exclude`.
- `poly fmt [PATHS]…` — apply formatting. `--check` is a dry run that reports drift without
  writing. `--fix` writes changes. `poly fmt` is a pure formatter — it never runs the
  whole-project lint phase.

## Flags

- `--format human|json|toon` — human is the default colored output; `json` and the compact
  `toon` variant are machine-readable. Under `--format json`/`toon`, `poly lint`'s
  whole-project section goes to stderr so stdout stays a single valid document — a machine
  consumer must check the **exit code**, not just the payload.
- `--exclude <glob>` — skip paths on top of `.gitignore`. Gitignore-style: a glob without a
  leading `/` matches a directory of that name at **any** depth (`e2e/**` also prunes
  `src/test/java/io/xberg/e2e/`), while a leading `/` anchors it to the config directory
  (`/e2e/**`). `poly doctor` warns when a rule matches at more than one depth.
- `--config <path>` — point at a specific `poly.toml`.
- `--no-cache` — bypass the blake3 content-hash result cache.
- `-j <N>` — parallelism; `--no-color` — plain output.
- `--deny-skips` / `--max-skips <N>` — strict coverage. A **skipped** file is one nothing
  inspected: a path named on the command line that no engine covers (`App.csproj`), or a file
  every routed backend declined (Go-templated YAML, a hash-stamped generated file). Skips are
  always reported and named; these flags make them fail the run (exit `2`), naming every file
  they fired on. `--verbose` lists every skip in `pretty` output; `--format json`/`toon`
  always carries the full set as entries with a `skipped` reason, so a consumer can assert on
  the set instead of parsing the summary.

## Exit codes

- `0` — clean (no findings, no drift).
- `1` — findings or formatting drift.
- `2` — an error (bad config, tool failure), or work the run could not verify: a missing path
  argument, a file the formatter could not parse, or a `--deny-skips`/`--max-skips` breach.

`poly lint` exits non-zero only on **error-severity** findings; warnings do not fail CI.

## The loop

1. `poly fmt --check . --format json` and `poly lint . --format json` — capture drift and
   findings, checking the exit code.
2. Read the JSON to see exactly which files and rules are involved.
3. `poly fmt --fix .` then `poly lint --fix .` to apply what is auto-fixable.
4. Re-run the checks; hand-fix whatever remains (exit code back to 0).
