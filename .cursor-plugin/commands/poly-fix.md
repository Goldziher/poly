---
priority: high
usage: "/poly-fix [paths]"
description: "Apply poly lint --fix and fmt --fix, then re-check and report what changed and what remains"
---

<!--
AI-RULEZ :: GENERATED FILE — DO NOT EDIT
Content-Hash: blake3:aefedcccf8e34cd75d79f5dfc885cdd00241b405b5e50461c42344db3c4d4a88
Source-Hash: blake3:0adf16213f8c0f77e494fd60f9136dd61420c257778d9df38c0bbca19b5f83e9
Schema-Version: v1
-->

# poly Fix

Apply poly's autofixes over `${1:-.}`, then verify.

1. `poly lint --fix ${1:-.}` — apply lint autofixes, and run the whole-project phase in **fix**
   mode (`cargo sort` in place, `cargo machete --fix`, `cargo clippy --fix --allow-dirty
   --allow-staged`; `cargo deny` has no autofix and stays check-only).
2. `poly fmt --fix ${1:-.}` — format files in place. `poly fmt` is a pure formatter and never
   runs the whole-project phase, in either direction; only step 1 does.
3. Re-check: `poly fmt --check ${1:-.} --format json` and `poly lint ${1:-.} --format json`.

The whole-project phase in step 1 runs only because the argument is the repository root (the
`.` default). Narrower paths make the run path-scoped — per-file autofixes only — unless you
add `--workspace`; `--no-workspace` suppresses the phase outright.

Report:

- What changed — files reformatted and lint rules auto-fixed.
- What remains — findings that need a hand-fix (auto-fix couldn't resolve them), grouped by
  rule and severity.
- The final exit status of step 3 (aim for `0`). Note that steps 1 and 2 legitimately exit
  `1` when they changed something; only `2` means the run failed to verify.

To apply no fixes, use `/poly-check` instead. (Even there poly's whole-project phase executes
`cargo clippy` and friends against the live worktree; add `--no-workspace` if the tree must be
left untouched.)
