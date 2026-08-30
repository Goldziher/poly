# Hooks reference

`poly hooks` is poly's native git-hook runner, configured entirely from `poly.toml [hooks]` (ADR
0012). Install the hooks once — they then run on every `git commit`:

```sh
poly hooks install
```

Hooks come from `poly.toml`: builtins, inline jobs, and optional local or Git producer catalogs.
Git refs are resolved into `poly-hooks.lock`; normal runs stay on the locked commit and
`poly hooks update` refreshes configured branches or tags.

Git catalogs share a global cache under `$XDG_CACHE_HOME/poly/hook-sources` (or the
platform cache directory). Poly keeps one URL-keyed bare mirror and immutable checkouts
keyed by commit. A per-source lock serializes fetch and materialization, so different
repositories can safely use the same catalog concurrently without duplicate clones.
Catalog hooks always execute from the consumer repository; the read-only producer checkout is
available through `POLY_HOOK_SOURCE_ROOT`. Local path sources bypass the global cache.

```toml
[[hooks.sources]]
id = "ai-rulez"
git = "https://github.com/acme/poly-hooks.git"
revision = "v4.9.0"
hooks = ["ai-rulez-validate"]

[[hooks.sources]]
id = "ai-rulez-dev"
path = "../ai-rulez"
hooks = ["ai-rulez-validate"]
```

Exactly one of `git` or `path` is required. Git sources require a revision and a committed lock;
local sources accept relative, parent-relative, or absolute paths, remain unlocked, and reload on
every run. The `hooks` list explicitly selects producer hook IDs, so new producer hooks never become
active without a consumer configuration change.

The producer alone owns `poly-hooks.toml`. It can publish multiple hooks and multiple guarded
execution paths for each hook:

```toml
version = 1

[[hooks]]
id = "ai-rulez-validate"
stages = ["pre-commit"]
args = ["generate", "--dry-run"]
files = [".ai-rulez/**", "**/.ai-rulez/**"]
workspace = true
pass_filenames = false

[[hooks.paths]]
channel = "npx"
check = "command -v npx"
run = "npx -y ai-rulez@latest"

[[hooks.paths]]
channel = "uvx"
check = "command -v uvx"
install = "uv tool install ai-rulez"
run = "uvx ai-rulez"
```

Every hook requires at least one path. Poly checks paths in the machine preference order and uses
the first whose `check` exits zero. An optional `install` command runs only during explicit
`poly hooks install`; ordinary hook runs use `run` directly, allowing commands such as `npx -y` or
`uvx` to self-provision. Poly does not fall through if installation or the selected command fails.
Machine-only preferences belong in gitignored `poly.local.toml`:

```toml
[hook_preferences]
channels = ["npx", "uvx", "system"]
```

Because that file is gitignored it never exists in a freshly created linked worktree, so poly also
looks for it beside the **main** worktree — the nearest file wins. To skip every poly hook for one
invocation, set `POLY_SKIP_HOOKS=1`; this is the supported escape hatch, since `git commit
--no-verify` bypasses `pre-commit` and `commit-msg` but never `prepare-commit-msg`. A
`prepare-commit-msg` run that cannot provision its external hook sources warns and continues rather
than blocking the commit; every other stage still treats that failure as fatal.

`poly hooks install` validates every selected hook path before installing Git shims. Treat producer
catalogs and their checks and commands as trusted code: they execute with your user permissions.
Normal runs never resolve Git refs or modify the lock; review changes and run `poly hooks update`
explicitly.

## Builtin hooks

| Builtin | Runs |
|---|---|
| `lint` | `poly lint` over the staged files |
| `fmt` | `poly fmt --check` over the staged files |
| `commit` | Conventional Commit + AI-trailer check on the commit message (`gitfluff`) |
| `file_safety` | Pure-Rust checks: merge-conflict markers, added large files, private keys, case conflicts, and shebang/executable parity |
| `cargo` | Whole-workspace `cargo clippy`, `cargo sort`, `cargo machete`, and `cargo deny` — each PATH-probed and skipped when absent |

The three file-scoped builtins (`lint`, `fmt`, `file_safety`) **inherit `[discovery] exclude`** —
a repo's excluded paths are stated once, not restated per hook. A hook's own `exclude` adds to
the inherited globs; `exclude_mode = "replace"` in the hook's table opts out and keeps only its
own:

```toml
[hooks.builtin.lint]
exclude = ["**/tags.rs"]      # effective: [discovery] exclude + **/tags.rs
```

Add an inline job for anything else — it wraps an existing script or task target, no plugin needed:

```toml
[hooks.pre-commit.scripts.docs]
script = "scripts/check-docs.sh"
runner = "bash"
files = "**/*.md"
```

## Per-file vs. whole-workspace hooks

Most hooks are **per-file**: they receive the staged file list and run on it. But some
tools analyze the *whole project* at once — `cargo clippy`, a type checker like `pyrefly`,
`mypy`, `tsc` — and can't be scoped to a file list. Mark those `workspace = true`:

```toml
[hooks.pre-commit.commands.pyrefly]
run = "pyrefly check packages/python"   # whole-package; no staged files appended
files = "packages/python/**/*.py"        # gate: only run when a Python file is staged
workspace = true
```

A `workspace = true` job takes no appended filenames (use a `{staged_files}` template to opt
back in). The `cargo` builtin group is whole-workspace automatically.

## Hook concurrency: `serial`

Hooks in a stage run **concurrently** on poly's rayon pool — whole-project hooks included,
since overlapping `cargo clippy` with `tsc` is where a run's wall-clock is won. `serial` is
the opt-out for a job that cannot tolerate a *peer* running at the same time:

```toml
[hooks.pre-commit.commands.migrate]
run = "./bin/migrate --check"
serial = true          # never beside another `serial = true` job

[hooks.pre-commit.commands.tests]
run = "cargo test --workspace"
workspace = true
serial = "cargo"       # never beside another member of the "cargo" set
```

`serial` names a **mutual-exclusion set**, not a stop-the-world: a serial job still runs
alongside every hook outside its set. `serial = true` joins the shared set; `serial = "<name>"`
joins a named one; `serial = false` opts out of both, overriding a stage-level
`parallel = false`.

The built-in **`cargo` group ships in the `"cargo"` set already** — nothing to configure.
Cargo serializes its own subcommands on the package-cache lock, and anything that builds on
the build-directory lock, so running `cargo clippy` / `sort` / `machete` / `deny` at once buys
no wall-clock and costs the queue its visibility: a blocked subcommand prints nothing while
its own timeout budget runs down (this is how a `cargo deny check` that takes 1.7s alone gets
killed at the 30-minute whole-project budget). A job whose `run` line invokes cargo joins the
set automatically; a **script** that shells out to cargo is invisible to poly and should name
`serial = "cargo"` itself.

A hook queued behind a set peer is not running, so its budget has not started — it can never
be killed for another hook's build time.

## Staged isolation

Every hook in a commit-gating run — per-file and whole-workspace alike — validates **one tree**:
a non-destructive snapshot of the git index, not the live worktree. Unlike `git stash`-based
approaches, your working tree is never touched. A run is staged-scoped or worktree-scoped as a
whole, never a mix — a per-file hook reading the worktree while a whole-workspace hook in the same
run reads the index is how a commit gate passes a commit whose staged content it never actually
saw. Every hook outcome records which tree produced its verdict, and the stage banner renders it
(`[stage] pre-commit — validated staged content`).

On by default for the commit-gating stages (`pre-commit`, `pre-merge-commit`); skipped for
`--all-files` and non-index stages, which check the worktree by design. Opt out for the whole run
with `isolate = false`:

```toml
[hooks]
isolate = false   # validate the live worktree instead of the staged snapshot
```

The snapshot is a persistent cache in the per-user cache dir
(`<platform-cache>/poly/<repo-key>/staged`, outside the repo), refreshed in place each run.
Content is sourced straight from the git **index blob** (never copied from the worktree),
so an unstaged edit can never leak in regardless of git's stat-cache state. A file is
re-materialized only when its staged object id changed since the last snapshot (tracked by a
`path → OID` manifest), so unchanged files are left untouched and cargo/pyrefly/`tsc` incremental
caches stay warm; files that left the tree are pruned while tool caches inside the snapshot are
preserved. It self-heals and is purgeable like any cache (`poly cache clean`).

A `stage_fixed` hook that rewrites its matched files writes into the snapshot, so the fix is
carried back to the worktree copy — but only where that copy is byte-identical to the index.
Where it differs, the author holds unstaged work the write-back must not silently overwrite or
stage; the fix is withheld instead, and for a `stage_fixed` hook that fails the run rather than
losing the unstaged edit. See [ADR 0019](../adrs/0019-staged-isolation-whole-workspace-hooks.md).

## Prerequisites: `precondition` and `before`

A hook can declare what must be true before it runs. Both keys exist at **stage** scope
(`[hooks.<stage>]`) and, preferably, at **hook** scope:

```toml
[hooks.pre-commit.commands.kotlin]
run = "./gradlew detekt"
workspace = true
precondition = "test -f gradlew"        # not applicable here -> visible skip, not a failure
before = "./gradlew --version"          # setup broke -> THIS hook's verdict is unknown
```

The two mean different things:

| key            | on failure                                | scope of the damage             |
| -------------- | ------------------------------------------ | -------------------------------- |
| `precondition` | hook **skipped** — it does not apply here  | not a failure                   |
| `before`       | hook's verdict is **unknown** — it did not run | fails the run                |

**Prefer the hook-scoped form.** A stage-scoped `precondition` withholds *every* hook in the
stage, and a stage-scoped `before` leaves every hook without a verdict. A hook-scoped one
contains the damage to the tool it guards, so the rest of the suite still validates.

Scope also decides **which tree** the prerequisite is evaluated against. A hook-scoped
prerequisite runs in the hook's own execution root — the **staged snapshot** for a
`workspace = true` hook under isolation, the worktree otherwise. Stage-scoped steps are not tied
to a hook and always run in the worktree.

A hook that does not run is always listed in the report with its reason — never silently
dropped:

```text
[stage] pre-commit
  - kotlin-not-applicable (precondition not met: test -f settings.gradle)
  ? kotlin-snippets (not run — setup failed in ~/.cache/poly/<key>/staged: ./gradlew --version)
      before: ./gradlew --version
      ERROR: Gradle wrapper jar missing
  ✓ rust
```

## Hook timeouts

Every process a run spawns — hook bodies, `before`/`after` steps, and `precondition` probes —
runs under a time budget. A wedged tool is killed (the whole process group: `SIGTERM`, then
`SIGKILL`) and reported as **killed**, which is deliberately not the same as failed: `×` means
the tool judged your code and said no, `⧖` means poly stopped it before it judged anything.
Either way the run fails — a hook that checked nothing must never report success.

Defaults are hang detectors, not performance budgets: 10 minutes per-file, 30 minutes for a
`workspace = true` hook (a cold `cargo clippy` is legitimately slow), 10 minutes for a
`before`/`after` step, and 60 seconds for a `precondition` probe. A hook still running after
15 seconds announces itself on stderr, then every minute, naming the hook and its kill
deadline.

Set a per-job budget with `timeout` — whole seconds, or a duration (`500ms`, `30s`, `10m`,
`1h`), or `0`/`off`/`none` to run it unbounded:

```toml
[hooks.pre-commit.commands.ai-rulez-validate]
run = "ai-rulez validate"
timeout = "90s"          # this tool is known to wedge; bound it tightly
```

```text
[stage] pre-commit — validated worktree
  ⧖ ai-rulez-validate (timed out: poly killed it after 90.2s, limit 90.0s)
  markers: ✓ passed  × failed  ⧖ killed by poly on timeout
```

Four environment variables override the budgets run-wide, taking the same values:
`POLY_HOOK_TIMEOUT`, `POLY_HOOK_WORKSPACE_TIMEOUT`, `POLY_HOOK_STEP_TIMEOUT`,
`POLY_HOOK_PRECONDITION_TIMEOUT`. Resolution is **environment override → `timeout` in
`poly.toml` → shape default**. Disabling restores the previous behaviour exactly: no deadline,
no liveness notice, no separate process group.

A cargo hook gets one extra protection, and it needs no configuration. Cargo serialises on
`$CARGO_HOME/.package-cache`, so a hook can sit blocked behind `rust-analyzer` or your own
`cargo build` without doing any work — and be killed for waiting. Before starting a hook in the
`cargo` exclusion set, poly checks that lock and, if somebody outside the run holds it, waits
for it to clear **before** the hook's clock starts. The wait is bounded by half the hook's own
budget (a hook with timeouts disabled never waits), and when that runs out the hook is started
anyway rather than withheld. See [ADR 0023](../adrs/0023-hook-timeouts-and-liveness.md) and
[ADR 0024](../adrs/0024-hook-concurrency-exclusion-sets.md).

## Hook exit codes

`poly hooks run` distinguishes three outcomes, so a CI job reading only the exit status can
tell a clean run from one that checked nothing:

| exit | meaning                                                                  |
| ---- | -------------------------------------------------------------------------- |
| `0`  | validated and clean (a hook with no matching files counts as validated)   |
| `1`  | a hook failed, or a `before` left a hook's verdict unknown                |
| `2`  | **validated nothing** — a `precondition` withheld every configured hook   |

## Conditional `skip` / `only`

`skip`/`only` accept a bare boolean or a list of `{ run = "<command>" }` conditions; a
condition is active when its command exits 0.

```toml
[hooks.pre-commit.commands.kotlin]
run = "./gradlew detekt"
only = [{ run = "test -f settings.gradle" }]
```

Only the `run` form is evaluated. Other lefthook condition forms (`ref = "..."`, bare
git-operation names like `"merge"`) are **rejected at config load** rather than accepted and
ignored — a guard that silently does nothing is worse than no guard.

## Hook caching

Hook results are cached (`[cache.results] hooks = "safe"` by default): a hook is **skipped
entirely** when its declared inputs are unchanged since the last passing run. The `cargo` group
is keyed on the Rust source/manifest set out of the box, so a commit touching no Rust skips
`clippy`/`sort`/`machete`/`deny` (opt out with `cargo = { cache = false }`). Give a custom
whole-workspace job the same treatment by declaring its inputs:

```toml
[hooks.pre-commit.commands.pyrefly]
run = "pyrefly check packages/python"
files = "packages/python/**/*.py"
workspace = true
cache = { inputs = ["packages/python/**/*.py", "pyproject.toml"] }
```

For workspace hooks the cache key is derived from **staged** content, so it stays correct under
isolation. For Rust compile times, enable `[cache.sccache]` to content-cache `rustc` output.

## Excluding the cargo group from `poly lint`

`poly lint` runs the `cargo` group as its whole-project phase (see
[the whole-project phase](#the-whole-project-phase-executes-tools)). To keep it as a
`pre-commit` gate but skip it in `poly lint` — e.g. a CI `validate` job whose plain checkout
cannot compile the workspace, while a dedicated job runs clippy — set `lint = false`:

```toml
[hooks.builtin.cargo]
lint = false   # runs in git hooks, excluded from `poly lint`'s whole-project phase
```

This is the per-group counterpart to `[lint] workspace = false`, which disables the whole-project
phase for **every** tool at once.

## The whole-project phase executes tools

`poly lint` without `--fix` applies no fixes of its own: poly's per-file tier only writes under
`--fix`, and the whole-project phase is asked for check mode. But that phase **executes the
configured tools against the live worktree**, and those tools are ordinary programs whose own
side effects poly neither requests nor controls — `cargo clippy` populates `target/` and can
refresh `Cargo.lock`, a `go` invocation can append to `go.work.sum`, a type checker can write its
own cache. So a plain `poly lint` can leave the tree changed, even though poly itself changed
nothing.

Three consequences worth knowing:

- **The phase is not path-scoped.** `poly lint <paths>` skips it entirely for that reason
  (`--workspace` opts back in). Under `--workspace` the tools still see the whole repository,
  regardless of the named paths and of `[discovery] exclude`; poly prints a note saying so.
- **`[discovery] exclude` does not apply to it.** It filters poly's own file discovery, not what a
  whole-project tool chooses to read.
- **Opting out makes the run fully read-only** (poly's per-file tier writes nothing without
  `--fix`): pass `--no-workspace`, or set `[lint] workspace = false`. Use one of them for a
  checkout that must stay pristine — a CI job that diffs the tree afterwards, or a gate on a
  read-only source tree.

### Applying whole-project fixes

Under `--fix`, the whole-project phase runs its tools in **fix mode**: `cargo sort` sorts in place,
`cargo-machete --fix` prunes unused dependencies, and `cargo clippy --fix --allow-dirty
--allow-staged` applies clippy autofixes (`cargo deny` has no autofix and stays check-only). Fix
mode is what `--fix` adds; the phase itself runs either way, so `--no-workspace` (not the absence
of `--fix`) is what skips it. `poly fmt` is a pure formatter and never runs the whole-project phase
(that phase is linting, not formatting). The git-hook / commit-gate path never requests fix mode,
so a commit is never silently auto-rewritten by it.
