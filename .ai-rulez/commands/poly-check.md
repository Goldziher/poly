---
priority: high
usage: "/poly-check [paths]"
description: "Lint and check formatting with poly — apply no fixes; summarize findings and drift"
---

# poly Check

Run poly as a checking gate over `${1:-.}`. Apply no fixes.

1. `poly fmt --check ${1:-.} --format json` — capture formatting drift.
2. `poly lint ${1:-.} --format json` — capture lint findings (remember the whole-project
   section is on stderr under `--format json`; check the exit code).

Step 2 applies no fixes, but it is not read-only: `poly lint`'s whole-project phase executes
the configured whole-project tools (`cargo clippy` and friends) against the live worktree, and
their own side effects — a refreshed lock file, a populated build or type-checker cache — are
not poly's to control. Add `--no-workspace` when the tree must be left untouched; that drops
the whole-project tools from the check, so say so in the report.

Report:

- Which files have formatting drift.
- Lint findings grouped by rule and severity (error vs warning).
- The overall pass/fail from the exit codes (`0` clean, `1` findings/drift, `2` error).

Do not apply fixes here — if the user wants them applied, run `/poly-fix`.
