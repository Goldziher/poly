---
priority: high
usage: "/poly-check [paths]"
description: "Lint and check formatting with poly — read-only, no writes; summarize findings and drift"
---

# poly Check

Run poly as a read-only gate over `${1:-.}`. Make no changes to the tree.

1. `poly fmt --check ${1:-.} --format json` — capture formatting drift.
2. `poly lint ${1:-.} --format json` — capture lint findings (remember the whole-project
   section is on stderr under `--format json`; check the exit code).

Report:

- Which files have formatting drift.
- Lint findings grouped by rule and severity (error vs warning).
- The overall pass/fail from the exit codes (`0` clean, `1` findings/drift, `2` error).

Do not apply fixes here — if the user wants them applied, run `/poly-fix`.
