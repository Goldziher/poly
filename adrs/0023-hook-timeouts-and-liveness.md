# 0023 — Hook Timeouts and Run Liveness

- Status: Accepted
- Date: 2026-08-12

## Context

A consumer reported a `pre-commit` run wedged by a single whole-project hook
(`ai-rulez:ai-rulez-validate`): roughly **25 minutes with zero output**, four commits
blocked, `--no-verify` the only way through, and the behaviour intermittent. The hang itself
is the third-party tool's defect. Everything that made it *undiagnosable* was ours:

1. **No time bound.** The runner called `Command::output()`, which waits forever. A hook that
   never returns hangs the commit for as long as the author is willing to wait.
2. **Nothing on screen before a hook is spawned.** The final report is rendered only after the
   whole run completes, so a run that never completes renders nothing at all. The author could
   not name the responsible hook, only the fact that "poly" was stuck.
3. **An overloaded status marker.** `-` marked a skipped hook, and there was no rendering at
   all for "started but not finished" — so a hook that never returned was indistinguishable
   from one that never applied.

The result is the failure mode this runner has been systematically closing: a check whose true
state ("running? wedged? skipped?") cannot be read off the output. It is the same class as a
false pass — the report does not say what actually happened.

## Decision

### 1. Every hook process runs under a time budget

`crates/poly-hooks/src/timeout.rs` resolves a [`Budget`] per hook: a `limit` past which poly
kills the process, and the cadence at which a still-running hook announces itself. The budget
applies to each **spawned process**, so an `ARG_MAX`-batched hook gets the full budget per
batch (batches run concurrently, so wall-clock is unchanged).

Resolution order: `Hook::timeout` (per hook) → environment override → shape default.

**Defaults are hang detectors, not performance budgets.** Too low a default converts a working
setup into a broken one, which would be a worse defect than the hang, so they differ by hook
shape:

| Hook shape | Default | Why |
|---|---|---|
| per-file (`workspace = false`) | **10 minutes** | Formatters and linters over a file batch finish in milliseconds to seconds; ten minutes is orders of magnitude of headroom. |
| whole-project (`workspace = true`) | **30 minutes** | `cargo clippy` on a cold `target/`, `tsc` on a large monorepo, a first `gradle` run — these legitimately run for many minutes, and killing a real cold build would be its own outage. |

Both are overridable run-wide via `POLY_HOOK_TIMEOUT` / `POLY_HOOK_WORKSPACE_TIMEOUT` (whole
seconds; `0`, `off`, or `none` disables the limit and restores the previous unbounded
behaviour exactly, including the un-supervised execution path).

### 2. A killed hook is a distinct status, not a generic failure

`HookStatus::TimedOut(TimeoutReason { limit, elapsed })` sits alongside the existing
`Skipped` / `Unknown` vocabulary rather than reusing `Failed`:

- `Failed` means *the tool judged your code and said no* — you fix the code.
- `TimedOut` means *poly stopped the tool before it judged anything* — you raise the budget or
  fix the wedged tool.

Conflating them would put the reader back where the hang left them. Like `Unknown`, a timeout
**is a failure** (`is_failure() == true`, so the run and the commit fail — a silent pass on a
hook that checked nothing is the exact false-pass class being eliminated) and **is not a
verdict** (`is_verdict() == false`, so it feeds `validated_nothing()` correctly: a killed hook
validated nothing).

### 3. The kill terminates the process tree

`crates/poly-hooks/src/supervise.rs` spawns each supervised child in **its own process group**
(Unix) and signals the group: `SIGTERM`, then `SIGKILL` after a 500 ms grace. Killing only the
direct `sh -c` would leave the real tool running and still holding whatever the hang was
holding — an orphan is strictly worse than the hang it replaced. Output is drained on
dedicated threads, which is not optional: a child that fills the pipe buffer while nobody
reads it blocks forever, reintroducing the hang through the back door.

### 4. Liveness: announce-on-threshold, not announce-always

A still-running hook prints `⋯ still running: <id> (15.0s elapsed, killed at 1800.0s)` to
stderr after **15 seconds**, then every **60 seconds**. Routed through the progress bar when
one exists (so it lands above the live spinners), straight to stderr otherwise — which is the
case that matters, since a non-interactive run has no spinner to reveal anything.

This was chosen over printing a line before *every* spawn: a pre-commit with 30 hooks would
become a wall of noise on every ordinary commit, and the noise would train people to ignore
exactly the line that matters. The threshold form keeps a normal run silent while guaranteeing
that a hang leaves its hook id — and its kill deadline — on screen, repeatedly, for as long as
it hangs.

### 5. Distinct markers, plus a legend

| Marker | Meaning |
|---|---|
| `✓` / `×` | passed / failed (the tool's own verdict) |
| `-` | skipped — did not apply |
| `⧖` | killed by poly on timeout |
| `?` | not run (setup failed) |
| `⋯` | still running (live notice only, never a final status) |
| `!` | nothing was validated (run summary) |

The report emits a `markers:` legend naming exactly the non-obvious markers it used, and
omits it entirely when every hook produced a plain verdict. The rendered text carries the
meaning independently of the glyph (`(timed out: poly killed it after 20.0s, limit 20.0s)`),
so a terminal without the glyph loses nothing.

## Consequences

### Positive

- A wedged hook can no longer block a commit indefinitely, and when it is killed the report
  names the hook, the elapsed time, the budget, and the fact that **poly** ended it.
- A hang is attributable *while it is happening*, not only in hindsight.
- Partial output captured before the kill is retained and rendered, which is often the only
  clue about where the tool wedged.
- Disabling the timeout restores the previous execution path byte for byte, so the escape
  hatch is total.

### Negative / risks

- **Ctrl-C during a long hook can orphan a child.** Children no longer share poly's process
  group, so a terminal `Ctrl-C` reaches poly but not them. poly kills the group on timeout and
  on a wait error, but not on its own interruption — `cleanup::cleanup()` exists for exactly
  this and is not yet wired to a signal handler in the binary. Follow-up: wire it (in
  `poly-cli`) and have the supervisor register live process groups.
- **Two drain threads per supervised process.** Short-lived and dominated by the process spawn
  itself, but it is real overhead the unbounded path did not have.
- **A legitimately slow hook can be killed** if a repo's cold build exceeds 30 minutes. The
  per-hook and environment overrides exist for this, and the 15-second heartbeat warns long
  before the kill.
- **Stage-level `before` / `after` steps and preconditions are still unbounded.** They run
  under `Budget::unlimited()`. A hang there is the same defect class and is deliberately left
  as follow-up rather than widened into this change.

### Not yet wired: `poly.toml` per-hook `timeout`

The model carries `Hook::timeout`, but lowering `poly.toml` → `Hook` lives in
`poly-workspace/src/lower.rs`, outside this change's scope. Until a `timeout` key is added to
`poly-config`'s `Job` and lowered there, the configurable surfaces are the two environment
variables and the programmatic `Hook::timeout`. A config key that parsed but was ignored would
be a false promise, so none was added.

## Alternatives considered

- **Fold timeouts into `HookStatus::Failed`.** Rejected: the two facts prompt different
  actions, and the whole point of this change is that the reader can tell states apart.
- **Reuse `HookStatus::Unknown(UnknownReason)`.** The concept fits ("no verdict, fails the
  run") but `UnknownReason` describes a failed `before` command in a named root; a timeout has
  no command and no root, only a limit and an elapsed time. The vocabulary is reused
  (`is_failure` / `is_verdict` semantics, the "named non-verdict" rendering shape) without
  distorting the type.
- **Announce before every spawn.** Rejected as noise; see decision 4.
- **A single global run timeout.** Rejected: it cannot name the responsible hook, which is the
  actual defect being fixed, and it would kill innocent hooks running in parallel.
- **Kill only the direct child.** Rejected: `sh -c 'a && b'` leaves `b` running; an orphan
  holding a lock is worse than the hang.
- **A tighter default (30–60s).** Rejected outright: a cold `cargo clippy` would be killed on
  the first commit of the day, and a linter that breaks working setups gets uninstalled.
