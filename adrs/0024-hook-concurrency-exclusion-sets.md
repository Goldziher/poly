# 0024 — Hook Concurrency and Exclusion Sets

- Status: Accepted
- Date: 2026-08-13

## Context

Two defects, and they are the same defect.

**1. One serial job serialized the whole stage.** A priority group ran under a single
boolean: `group.iter().any(|hook| hook.is_serial())` — if *any* member could not run beside a
peer, *every* member ran alone. `Hook::is_serial()` is `require_serial || !parallel`, and
`parallel` is lowered from the stage-level `[hooks.<stage>] parallel` key, whose schema
default is `false`. So a repo that declared a single inline job without writing
`parallel = true` — poly's own `poly.toml` among them — ran its entire pre-commit stage one
hook at a time: `lint`, `fmt`, `file-safety`, all four cargo tools, every script, sequentially,
on a machine with idle cores. The per-file tier inside `poly lint` was fully parallel; the
tier that spawns the *expensive* tools was not.

**2. Concurrent cargo hooks queue invisibly, and the queue is charged to the wrong hook.**
The cargo builtins (`cargo-clippy` / `-sort` / `-machete` / `-deny`) are lowered with
`parallel = true`, so in any group without a serial member they run concurrently. Cargo does
not let them: every subcommand takes cargo's **package-cache** lock, and anything that builds
takes the **build-directory** lock for the whole build. The measured behaviour (macOS, this
repo):

- Two `cargo build`s sharing one target dir: the second prints `Blocking waiting for file lock
  on build directory` and waits for the entire first build.
- With an exclusive lock held on `$CARGO_HOME/.package-cache`, `cargo deny check licenses`
  blocks for as long as the lock is held and prints **nothing at all** — no message, no
  output, no exit. Standalone it takes 1.7 seconds.
- `cargo metadata` in the same state prints `Blocking waiting for file lock on package cache`.
  `cargo-machete` (manifests only) is unaffected.

So concurrency buys nothing here — the tools serialize regardless — and it costs three things:
the queue is invisible, poly's liveness line says "still running" about a process that is
doing nothing, and each blocked hook burns *its own* time budget while waiting for somebody
else's build. That is the mechanism behind a `cargo deny check` that takes 1.7 seconds being
reported `TimedOut` at the 30-minute whole-project budget (ADR 0023) on a cold `target/`: it
never ran. The lock holder can be another hook in the same run **or** a cargo process outside
it (a `rust-analyzer` check, a developer's build); this ADR removes the first case and makes
the second the only one left.

Fixing (1) without (2) makes (2) worse — more concurrency means more contention.

## Decision

### 1. Hooks run concurrently; the unit of exclusion is a **named set**

`Hook::serial_group: Option<String>`. Two hooks naming the same set never overlap. Hooks in
different sets, or in none, are unconstrained. `require_serial` / `parallel = false` keep
working: they map onto the shared set `SHARED_SERIAL_GROUP` (`"serial"`).

Serial means *"not beside a peer"*, never *"the run stops for me"*. A `serial` job still runs
alongside every hook outside its set — which is the whole difference from the old boolean.

### 2. The scheduler runs **chains**, not hooks

A priority group is partitioned into chains (`runner::exclusion_chains`): all members of one
set form a single chain in config order; every unconstrained hook is a chain of one. The
group's rayon `par_iter` runs over the *chains*, and a chain's members run one after another
on the worker that picked it up.

Still one rayon pool — the run's existing pool, sized by `effective_concurrency` — and still
no raw threads, no `tokio`. Chains, not mutexes: a mutex would park a rayon worker on a lock,
and a parked worker is a worker that is not running somebody else's hook.

Group order is preserved *within* a chain, which is what a `piped` stage relies on. Across
chains there is no order, exactly as before for a parallel group. `run_group` still returns
its hooks in group order, so `fail_fast`, `stage_fixed` write-back, cache stores, and report
ordering are untouched.

### 3. Queueing is poly's, so the budget stays honest

A hook waiting for its chain predecessor **has not been spawned**. Its clock starts when it
starts. Two properties fall out, and both are requirements rather than side effects:

- A queued hook can never be killed for a peer's build time.
- A queued hook never announces itself as "still running", because there is no process to
  announce. The liveness line keeps describing a process that is actually executing.

This is why the exclusion set is preferable to leaving the queue inside cargo: cargo's queue
is invisible to poly, so poly cannot report it and cannot avoid charging for it.

### 4. The cargo group ships serial; everything whole-project ships parallel

- The `cargo` builtins join `CARGO_SERIAL_GROUP` (`"cargo"`) during lowering — **default, not
  opt-in**. They still run beside non-cargo hooks.
- An inline job whose `run` line invokes `cargo` (anywhere: after `&&`, in a pipeline, by
  absolute path) joins the same set automatically. Over-inclusive on purpose: the cost of a
  false positive is that a job waits in a queue poly can see instead of one it cannot.
- A `workspace = true` job is **concurrent by default** — the stage-level `parallel` flag was
  never about whole-project tools, and overlapping them is where the wall-clock is won.
- A per-file job keeps following the stage's `parallel` flag, unchanged.

### 5. The `serial` job key

```toml
serial = true       # join the shared set
serial = "cargo"    # join a named set
serial = false      # explicitly concurrent; overrides `parallel = false` and the cargo rule
```

Absent is distinct from `false` (`Serial::Unset` vs `Serial::Off`): the first falls through to
the stage default and the cargo rule, the second is the escape hatch and has to survive both.
Bool-or-string mirrors the schema's existing habit (`timeout`, `Patterns`, `Guard`).

## Consequences

### Positive

- A stage with one serial job no longer serializes its builtins — the common case, since
  `parallel` defaults to `false`.
- Whole-project tools overlap: `cargo` (one chain) beside `tsc` beside `pyrefly`.
- The cargo queue is explicit, ordered, and correctly billed. The 1.7-second tool killed at
  30 minutes cannot recur from poly-internal contention.
- No new pool, no new threads, no config required for correct cargo behaviour.

### Negative / risks

- **A stage-level `parallel = false` no longer stops the world.** Its jobs still run one at a
  time relative to each other, but builtins now run alongside them. This is the intended
  reading of "serial", and `piped` ordering is unchanged.
- **The cargo heuristic can over-match** (`echo "no cargo here"`). It costs parallelism, never
  correctness, and `serial = false` opts out.
- **A script job is opaque.** poly cannot see that `scripts/hooks/rustdoc-lint.sh` runs
  `cargo doc`; such a script must name `serial = "cargo"` itself (this repo's `poly.toml`
  does).
- **External cargo processes still contend.** A `rust-analyzer` `cargo check` or a developer's
  own build holds the same locks, and a hook blocked behind *those* is still charged for the
  wait — poly does not own that queue and cannot shorten it. It is no longer reported as "still
  running", though: the supervisor watches for cargo's `Blocking waiting for file lock` line
  and the liveness notice says the hook is queued on that lock rather than working (ADR 0023).
  That is a reporting fix in the reporter/supervisor, not a scheduling one. The scheduling fix
  — probing the lock before spawning, so the wait happens before the clock starts, exactly as
  it does for a hook queued behind a peer in this set — has been measured and is viable; see
  "Probing cargo's lock before spawning" in ADR 0023 for what it does and does not buy. It
  belongs to this ADR's machinery, since only the exclusion set knows which hooks are cargo
  hooks.

## Alternatives considered

- **A mutex per exclusion set inside the existing `par_iter`.** Simpler to write, but it parks
  rayon workers on a lock and reintroduces the thing being fixed one level down: a hook
  waiting on the mutex has already been spawned in the eyes of any future budget accounting.
- **Raise the whole-project default above 30 minutes.** Treats the symptom. The tool was not
  slow; it was not running.
- **Exclude lock-wait time from the budget.** Detecting the wait from inside a child poly did
  not write is not possible in general — `cargo deny` printed nothing at all while blocked — and
  detecting it from what the child *says* is worse than nothing, since a hook could then talk
  itself out of its own timeout. poly can observe cargo's lock directly (a non-blocking `flock`
  probe on `$CARGO_HOME/.package-cache`; measured in ADR 0023), but the sound use of that is to
  wait **before** spawning, not to stop the clock on a process already running.
- **Serialize all whole-project hooks.** Correct for cargo, wrong for everyone else: `tsc` and
  `pyrefly` share no lock with cargo or with each other.
- **A global serial flag rather than named sets.** Would put an unrelated `serial = true` job
  in the same queue as the cargo tools, for no reason other than that both asked to be alone.
