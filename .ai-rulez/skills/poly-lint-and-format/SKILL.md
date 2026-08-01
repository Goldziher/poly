---
priority: medium
description: "Running poly lint / poly fmt — --fix, --format json|toon, --exclude, --config, exit codes, and the check → read-json → fix → re-check loop"
---

# poly Lint and Format

## Commands

- `poly lint [PATHS]…` — run the linters. `--fix` applies autofixes (and the whole-project
  fix phase). `--no-workspace` restricts to the per-file tier.
- `poly fmt [PATHS]…` — apply formatting. `--check` is a dry run that reports drift without
  writing. `--fix` writes changes. `poly fmt` is a pure formatter — it never runs the
  whole-project lint phase.

## Flags

- `--format human|json|toon` — human is the default colored output; `json` and the compact
  `toon` variant are machine-readable. Under `--format json`/`toon`, `poly lint`'s
  whole-project section goes to stderr so stdout stays a single valid document — a machine
  consumer must check the **exit code**, not just the payload.
- `--exclude <glob>` — skip paths on top of `.gitignore`.
- `--config <path>` — point at a specific `poly.toml`.
- `--no-cache` — bypass the blake3 content-hash result cache.
- `-j <N>` — parallelism; `--no-color` — plain output.

## Exit codes

- `0` — clean (no findings, no drift).
- `1` — findings or formatting drift.
- `2` — an error (bad config, tool failure).

`poly lint` exits non-zero only on **error-severity** findings; warnings do not fail CI.

## The loop

1. `poly fmt --check . --format json` and `poly lint . --format json` — capture drift and
   findings, checking the exit code.
2. Read the JSON to see exactly which files and rules are involved.
3. `poly fmt --fix .` then `poly lint --fix .` to apply what is auto-fixable.
4. Re-run the checks; hand-fix whatever remains (exit code back to 0).
