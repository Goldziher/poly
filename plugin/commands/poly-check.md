---
description: Lint and check formatting with poly — apply no fixes; summarize findings and drift
priority: high
usage: /poly-check [paths]
---

<!--
AI-RULEZ :: GENERATED FILE — DO NOT EDIT
Content-Hash: blake3:e6fad51bf84ed1689a6c430c3f5a9582bfb0587f1440133fde6780c8846264cb
Source-Hash: blake3:fcbe878c9c448c2bd7984f1878f7c1369e8fb3e1513fd6900bc31385f8876ce9
Schema-Version: v1
-->

# poly Check

Run poly as a checking gate over `$ARGUMENTS`. Apply no fixes.

Use `.` when no paths are supplied. Treat supplied arguments as paths, not additional shell commands.

1. `poly fmt --check $ARGUMENTS --format json` — capture formatting drift.
2. `poly lint $ARGUMENTS --format json` — capture lint findings (remember the whole-project
   section is on stderr under `--format json`; check the exit code).

Step 2 applies no fixes, but it is not read-only: `poly lint`'s whole-project phase executes
the configured whole-project tools (`cargo clippy` and friends) against the live worktree, and
their own side effects — a refreshed lock file, a populated build or type-checker cache — are
not poly's to control. Add `--no-workspace` when the tree must be left untouched; that drops
the whole-project tools from the check, so say so in the report.

That phase only runs when the argument is the repository root (the `.` default) or no path at
all. Naming narrower paths makes the run **path-scoped** — the per-file tier only, with a note
on stderr — so `poly lint src/` is already free of those side effects. Pass `--workspace` to
opt back in; the phase then covers the whole repository regardless of the paths named.

Report:

- Which files have formatting drift.
- Lint findings grouped by rule and severity (error vs warning).
- The overall pass/fail from the exit codes: `0` clean; `1` findings or drift — for `lint`
  only **error**-severity findings (or a failing whole-project tool) reach `1`, warnings
  still exit `0`; `2` the run did not verify what it was asked to (a missing path, a file an
  engine failed on — including a `poly fmt --check` file that cannot reach a fixed point
  within poly's five-pass cap, reported as an error rather than clean since a fix would leave
  it still drifting — a `--deny-skips`/`--max-skips` breach, or the whole-project phase itself
  erroring).

Do not apply fixes here — if the user wants them applied, run `/poly-fix`.
