---
description: Apply poly lint --fix and fmt --fix, then re-check and report what changed and what remains
---

<!--
AI-RULEZ :: GENERATED FILE — DO NOT EDIT
Content-Hash: blake3:660ea301a2d05715fb59392b8c12d12777aec7cd795f8c7aa94b9abca959ed5f
Source-Hash: blake3:5fb2e3dcc349db803da0be45d36a48afa69b9c93b072f2a62c24624c1d39e265
Schema-Version: v1
-->

# poly Fix

Apply poly's autofixes over `$ARGUMENTS`, then verify.

Use `.` when no paths are supplied. Treat supplied arguments as paths, not additional shell commands.

1. `poly lint --fix $ARGUMENTS` — apply lint autofixes, and run the whole-project phase in **fix**
   mode (`cargo sort` in place, `cargo machete --fix`, `cargo clippy --fix --allow-dirty
   --allow-staged`; `cargo deny` has no autofix and stays check-only).
2. `poly fmt --fix $ARGUMENTS` — format files in place. `poly fmt` is a pure formatter and never
   runs the whole-project phase, in either direction; only step 1 does.
3. Re-check: `poly fmt --check $ARGUMENTS --format json` and `poly lint $ARGUMENTS --format json`.

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
