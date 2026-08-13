# 0023 — Hook Timeouts and Run Liveness

- Status: Accepted
- Date: 2026-08-12 (amended 2026-08-13: the two deferred gaps are closed; a killed hook's
  honesty now rests on the scheduling property in ADR 0024. Amended again 2026-08-13: the
  output preview is fed while the hook runs, the external-lock residual is re-examined against
  a measured lock probe, and that probe is now wired — a cargo hook waits out cargo's
  package-cache lock, bounded and reported, before its budget starts)

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

Resolution order: **environment override → explicit per-hook budget → shape default** (see
"Precedence" below; the amendment reversed the first two).

**Defaults are hang detectors, not performance budgets.** Too low a default converts a working
setup into a broken one, which would be a worse defect than the hang, so they differ by what is
being run:

| Spawned process | Default | Why |
|---|---|---|
| per-file hook (`workspace = false`) | **10 minutes** | Formatters and linters over a file batch finish in milliseconds to seconds; ten minutes is orders of magnitude of headroom. |
| whole-project hook (`workspace = true`) | **30 minutes** | `cargo clippy` on a cold `target/`, `tsc` on a large monorepo, a first `gradle` run — these legitimately run for many minutes, and killing a real cold build would be its own outage. |
| `before` / `after` step (stage or hook scope) | **10 minutes** | Setup is bounded by dependency installation (`npm ci`, a wrapper download), not by compiling the workspace, so it takes the per-file number rather than the whole-project one. |
| `precondition` probe | **60 seconds** | `test -f gradlew` / `command -v cargo` answer instantly; a probe that needs minutes is not a probe. 60s still covers a network-touching probe (`gh auth status`, `docker info`) on a bad link, while bounding a wedged one inside a minute — which matters most here, because a *stage* precondition gates every hook in the stage. |

Each is overridable run-wide via `POLY_HOOK_TIMEOUT`, `POLY_HOOK_WORKSPACE_TIMEOUT`,
`POLY_HOOK_STEP_TIMEOUT`, and `POLY_HOOK_PRECONDITION_TIMEOUT`. Every timeout surface —
these four variables and the `poly.toml` `timeout` key — accepts one grammar, parsed by one
function: whole seconds (`90`), a suffixed duration (`500ms`, `30s`, `10m`, `1h`), or `0` /
`off` / `none` to disable. Disabling restores the previous unbounded behaviour exactly,
including the un-supervised execution path.

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

### 4a. The output preview is fed while the hook runs

*(added 2026-08-13)*

The spinner's rolling preview was fed from `Cmd::output_with_sink_supervised` **after** the
child exited: the supervisor captured everything on its drain threads and handed the whole
buffer to the sink in one pass at the end. A hook that runs for two minutes therefore showed an
empty preview for two minutes and then all of its output at once — which is precisely the
state this ADR exists to eliminate. The preview could not distinguish a hook doing work from a
hook doing nothing, so it answered the same question the liveness notice had to be added to
answer, and answered it wrongly.

`supervise::run_streaming` now hands the sink every byte as it arrives. Two properties are
load-bearing and are asserted by tests rather than assumed:

- **The captured output is byte-identical.** The drain threads still accumulate each pipe in
  full; the sink is given a view of what has arrived, never a copy that could diverge. Each
  stream reaches the sink in its own order, once, with the two pipes interleaved by arrival —
  the same interleaving a terminal would have shown. Nothing is dropped, repeated, or
  reordered within a stream.
- **The sink runs on the supervising thread, never on a drain thread.** The alternative —
  calling the sink from the two drain threads — needs a lock around the sink, and that lock is
  held across a terminal draw. A drain thread stalled behind a redraw is a pipe nobody is
  emptying, and a full pipe blocks the child forever: the hang, reintroduced through the same
  back door draining exists to close. The supervisor instead copies out whatever arrived on
  each poll (10 ms) and feeds it with no pipe lock held. This also bounds the UI cost — a
  torrential hook costs at most one sink call per pipe per poll, where the post-mortem burst
  cost one per 4 KiB — and it lets a sink stay single-threaded, which is why `PreviewSink`
  needs no synchronisation. The `run_streaming` signature carries no `Send` bound on the sink,
  so the guarantee is enforced by the type system and not merely documented.

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
- **A hook blocked on a lock held outside the run is still charged for the wait.** See
  "A killed hook must really have been running" below.

### Closed by the 2026-08-13 amendment

Both gaps this ADR originally deferred now ship:

- **Every spawned process is bounded.** Stage-level and per-hook `before` / `after` steps run
  under `timeout::step_budget()`, and both stage-level and per-hook `precondition` probes
  under `timeout::precondition_budget()` (`poly-hooks/src/runner.rs`). Nothing a run spawns is
  left on `Budget::unlimited()` unless a budget was explicitly disabled.
- **The `poly.toml` per-job `timeout` key is wired.** `poly-config`'s `Job::timeout` accepts a
  duration string or whole seconds, and `lower::job_timeout` resolves it into
  `Hook::timeout` — through the same parser as the environment overrides, so the two grammars
  cannot drift. A malformed value is a hard error naming the job, never a silently discarded
  budget.

### A killed hook must really have been running

`TimedOut` is only honest if a hook cannot be sitting in a queue while its clock runs. That is
a **scheduling** property, and it is why ADR 0024 (hook concurrency and exclusion sets) is
load-bearing for this one:

- A hook waiting for a peer in its exclusion set is **not spawned**, so its budget has not
  started and it announces nothing. Queueing that poly owns is free of charge.
- The cargo builtins ship in one exclusion set for exactly this reason. Concurrent cargo
  subcommands block on cargo's package-cache and build-directory locks — measurably, and in
  the `cargo deny` case with **no output at all** while blocked — so before ADR 0024 a
  1.7-second `cargo deny check` could be reported `TimedOut` at the 30-minute whole-project
  budget without ever running.

What remains: a hook can still block on a lock held by a process **outside** the run — a
`rust-analyzer` `cargo check`, a developer's own build, another repository's CI step sharing
`CARGO_HOME`. The common shape of that — the lock already held when the run reaches its cargo
hooks — is now waited out before the hook is spawned (see "Probing cargo's lock before
spawning" below); everything the probe cannot catch is still **charged** to the hook.
It is no longer *misreported* as one, though: the supervisor watches the child's output for
cargo's `Blocking waiting for file lock on <resource>` line, and while that is the last thing
the hook said, the liveness notice reads `⏸ waiting on a lock: <id> … — blocked on cargo's
<resource> lock held by a process outside this run, doing no work; the time budget is still
counting` instead of `⋯ still running`. The next line of real output clears it, so the notice
never claims a wait that has ended. Pausing the budget for such a wait was deliberately **not**
done — poly cannot verify a queue it does not own, and a hook that stops being charged on the
strength of one line the child printed is a timeout that can be talked out of firing. The
budget overrides remain the escape hatch.

### Probing cargo's lock before spawning: measured, then wired

*(added 2026-08-13; wired the same day)*

The shape that *would* fix the residual honestly is the one the exclusion sets already use:
move the wait to **before** the clock starts. If poly can tell that cargo's lock is held before
it spawns a cargo hook, it can wait for the lock to clear un-charged, exactly as a hook queued
behind a peer in its exclusion set waits un-charged. Unlike pausing the budget on the printed
line, nothing about it can be induced by the child's own output.

The question was whether poly can observe that lock without interfering with cargo's use of it.
Measured on macOS with cargo 1.97.1:

| What was tested | Result |
|---|---|
| Exclusive `flock` held on `$CARGO_HOME/.package-cache`, then `cargo metadata` | Blocked for the full 7.5 s hold, printed `Blocking waiting for file lock on package cache`, then proceeded. Reproduces with a dependency-free crate, so *any* cargo subcommand queues. |
| **Shared** `flock` held on the same file, then `cargo metadata` | Also blocked, for the full hold. Cargo asks for the lock exclusively, so "any holder at all" is the right question to ask. |
| Non-blocking exclusive probe (`flock(LOCK_EX\|LOCK_NB)`) while a lock was held | Reported held (`EWOULDBLOCK`) on every one of 120 samples. |
| The same probe opened `O_RDONLY`, no `O_CREAT` | Works — `flock` ignores the open mode — so the probe neither creates `$CARGO_HOME` entries nor needs write access, and a missing file reads as "free". |
| 2 380 probes at 400/s racing a real `cargo metadata --offline` on this repo | Observed the holder (181 samples held), and cargo completed cleanly, exit 0, with no blocking notice. |
| Exclusive `flock` held on `<target-dir>/debug/.cargo-lock`, then `cargo build` | Blocked for the full hold and printed `Blocking waiting for file lock on artifact directory`. Same mechanism, second lock. |

So the probe is **sound and safe**: `flock` is advisory and per-open-file-description, poly is a
different process from the holder, and a read-only open/try/close cannot drop or corrupt
anyone else's lock. The only cost is a microsecond-wide window in which poly itself holds the
lock, which a concurrent cargo would see as a brief block — not observed once in 2 380
attempts.

What it does **not** buy, and why it is a mitigation rather than a fix:

- **The wait moves, it does not vanish.** Between the probe and the spawn, an external cargo
  can take the lock; and cargo takes the package-cache lock at several points in a run, not
  only at startup, so a hook can still be charged for a wait that begins after it started.
- **The pre-spawn wait needs its own bound.** Waiting indefinitely for a lock poly does not own
  reintroduces the unbounded hang this ADR exists to bound. It needs a bound of its own and, on
  expiry, the graceful degradation of spawning anyway — which is today's behaviour, so the
  worst case is no worse.
- **It needs to know which hooks and which locks.** The package cache is one file and is easy;
  the artifact-directory lock is `<target-dir>/<profile>/.cargo-lock`, and both the target dir
  (poly may inject `CARGO_TARGET_DIR`) and the profile have to be guessed from a shell line.
  Guessing wrong is silent — no protection, no error.
- **Only cargo hooks may take this path.** Making every hook wait on cargo's lock would be a
  regression. The classification already exists as the `serial = "cargo"` exclusion set
  (ADR 0024), which is the correct place to hang it — *not* the supervisor, which sees only an
  argv and would have to sniff `cargo` out of a shell line.

#### What shipped

`poly-hooks/src/cargo_lock.rs`, called from `runner::run_hook` immediately before the hook's
batches are spawned and immediately before its clock starts:

1. **Only cargo hooks wait.** The gate is `Hook::is_cargo()` — membership of the ADR 0024
   `serial = "cargo"` exclusion set, decided during lowering where the command line is still
   understood. The supervisor is not involved and never sniffs an argv for `cargo`.
2. **Only the package-cache lock is probed.** `$CARGO_HOME/.package-cache`, opened `O_RDONLY`
   with no `O_CREAT`, `flock(LOCK_EX|LOCK_NB)`, released immediately. A missing file, an
   unreadable one, or a non-Unix build all read as "nothing to wait for": poly must never
   withhold a hook because it could not see a lock. The artifact-directory lock is
   **deliberately not probed** — see the unmitigated list below.
3. **The bound is derived, not configured.** `WaitPlan::for_budget` gives the wait *half* the
   hook's own resolved budget (`LOCK_WAIT_BUDGET_DIVISOR`). No new config key: the question
   "how long is it worth delaying this hook?" is already answered by how long the hook may run,
   and a knob nobody sets is protection nobody gets. Half rather than all, so a hook that waits
   out the whole bound and then overruns is still killed within 1.5× its configured limit. A
   hook whose budget is **disabled** does not wait at all — that escape hatch promises the
   pre-timeout path exactly, and an unbounded hook has no clock to protect.
4. **The wait is visible, and is its own state.** After two seconds (`LOCK_WAIT_ANNOUNCE_AFTER`,
   far shorter than the 15-second still-running threshold, because poly deliberately holding a
   hook back is not the normal case) the run prints `⏸ waiting to start: <id> (<n> waited,
   starting anyway at <bound>) — cargo's package cache lock is held by a process outside this
   run; the hook has not been spawned and its time budget has not started`. It shares the `⏸`
   marker and the vocabulary of the post-spawn notice, and contradicts it on the one point that
   differs: this hook has no clock running. Four states now read differently — queued before
   spawn, running, queued after spawn, killed.
5. **On expiry the hook is spawned anyway**, and the post-spawn notice takes over. Refusing to
   run it would turn somebody else's `cargo build` into a failed commit, and a check that did
   not run must never be reported as a pass; running late is the lesser harm. A `warn!` records
   that the bound expired, so the late start is not silent.

#### What remains unmitigated

- **The probe/spawn race.** The lock can be taken in the window between the probe and the
  spawn, and cargo re-acquires the package cache during a run, not only at startup.
- **The artifact-directory lock** (`<target-dir>/<profile>/.cargo-lock`) — the one a whole
  `cargo build` holds for its full duration — is not probed at all, because both the target
  directory (poly may inject `CARGO_TARGET_DIR`) and the profile would have to be guessed from
  a shell line, and guessing wrong is silent.

For both, the post-spawn liveness notice remains the report, and the budget overrides remain
the escape hatch. This is a **mitigation of the common case, not a fix**, and neither the code
nor the report claims otherwise.

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
- **Pause the budget when the child prints cargo's lock-wait line.** Rejected, and the
  rejection stands after the probe measurements above. The signal is a line the supervised
  process itself prints, so any hook could exempt itself from its timeout by echoing it, and a
  tool that wedged immediately after printing it would never be killed. Verifying the claim
  against a real `flock` probe removes the first objection but not the second, and the
  pre-spawn wait gets the same benefit without either.
- **Acquire cargo's lock and hand it to the child.** Rejected: cargo takes the lock itself, so
  poly holding it would deadlock the very hook it was protecting. Only a *probe* — try, then
  release immediately — is safe.
