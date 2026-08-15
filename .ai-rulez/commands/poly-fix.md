---
priority: high
usage: "/poly-fix [paths]"
description: "Apply poly lint --fix and fmt --fix, then re-check and report what changed and what remains"
---

# poly Fix

Apply poly's autofixes over `${1:-.}`, then verify.

1. `poly lint --fix ${1:-.}` — apply lint autofixes (and the whole-project fix phase).
2. `poly fmt --fix ${1:-.}` — format files in place.
3. Re-check: `poly fmt --check ${1:-.} --format json` and `poly lint ${1:-.} --format json`.

Report:

- What changed — files reformatted and lint rules auto-fixed.
- What remains — findings that need a hand-fix (auto-fix couldn't resolve them), grouped by
  rule and severity.
- The final exit status (aim for `0`).

To apply no fixes, use `/poly-check` instead. (Even there poly's whole-project phase executes
`cargo clippy` and friends against the live worktree; add `--no-workspace` if the tree must be
left untouched.)
