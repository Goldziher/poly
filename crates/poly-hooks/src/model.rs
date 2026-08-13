//! The in-memory hook model and the runner's request/outcome types.
//!
//! This is the B1 model: a [`Hook`] is a single subprocess invocation, a
//! [`StageSpec`] groups the hooks for one git stage with its
//! `precondition`/`before`/`after` steps, and [`HookRunRequest`] /
//! [`HookRunOutcome`] are the public entry/exit shapes for [`crate::run`].
//!
//! There is no YAML, no repo, and no provisioning here — config lowering
//! (poly.toml → `Vec<StageSpec>`) is Workstream B3 and lives in `poly-cli`.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use poly_cache::ResultCache;

use crate::filter::FilePattern;
use crate::identify::TagSet;
use crate::stage::Stage;
use crate::timeout::HookTimeout;

/// How a hook participates in tier-1 result caching.
///
/// The runner only ever stores an entry for a **passing, tree-clean** run, so a
/// cache hit always means "passed without modifying its inputs".
#[derive(Debug, Clone, Default)]
pub enum HookCache {
    /// Never cached.
    #[default]
    Disabled,
    /// Cache keyed by the content digest of the hook's matched files.
    MatchedFiles,
    /// Cache keyed by the content digest of these declared input globs
    /// (resolved against the whole tracked tree, not just the changed set).
    DeclaredInputs(FilePattern),
}

impl HookCache {
    /// Whether this policy permits the hook to be cached at all.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        !matches!(self, HookCache::Disabled)
    }
}

/// What a [`Hook`] executes.
#[derive(Debug, Clone)]
pub enum HookCommand {
    /// A shell command line, run via `sh -c` (`cmd /C` on Windows). Matched
    /// files and `args` are appended as positional arguments (`"$@"`).
    Run(String),
    /// A script file, optionally interpreted by `runner` (e.g. `bash`); when
    /// `runner` is `None` the script is executed directly.
    Script {
        /// Path to the script file.
        path: String,
        /// Interpreter program, if any.
        runner: Option<String>,
    },
}

impl Default for HookCommand {
    fn default() -> Self {
        Self::Run(String::new())
    }
}

/// The exclusion set a hook joins by setting [`Hook::require_serial`] (or
/// `parallel = false`): "not concurrent with another serial hook", which is
/// weaker than "alone in the run".
pub const SHARED_SERIAL_GROUP: &str = "serial";

/// The exclusion set every cargo invocation belongs to.
///
/// Cargo serializes its own subcommands on the package-cache lock and, for
/// anything that builds, on the build-directory lock — so concurrent cargo hooks
/// buy no wall-clock and cost the queue its visibility: a blocked hook prints
/// nothing while its own budget runs down.
pub const CARGO_SERIAL_GROUP: &str = "cargo";

/// One runnable unit within a stage — a single subprocess invocation.
///
/// Mirrors the poly.toml `Job` shape but carries only what the runner needs:
/// no globs-as-strings (patterns are pre-compiled into [`FilePattern`]), no
/// cache declaration (Workstream C), no `skip`/`only` guards (resolved during
/// lowering in B3).
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Default)]
pub struct Hook {
    /// Stable, human-readable identifier (used for output grouping).
    pub id: String,
    /// The git stage this hook belongs to.
    pub stage: Stage,
    /// The command to execute.
    pub command: HookCommand,
    /// Extra arguments appended before the matched files.
    pub args: Vec<String>,
    /// Environment variables injected on top of the inherited environment.
    pub env: BTreeMap<String, String>,
    /// Working directory override (relative to the repo root). `None` means
    /// the hook runs from the repo root, matching the existing behaviour.
    pub cwd: Option<PathBuf>,
    /// Include filter; `None` means "no filename constraint".
    pub files: Option<FilePattern>,
    /// Exclude filter; `None` means "exclude nothing".
    pub exclude: Option<FilePattern>,
    /// ALL of these file-type tags must be present.
    pub types: Option<TagSet>,
    /// AT LEAST ONE of these tags must be present.
    pub types_or: Option<TagSet>,
    /// NONE of these tags may be present.
    pub exclude_types: Option<TagSet>,
    /// Lower runs first; hooks sharing a `priority` form a parallel group.
    pub priority: i64,
    /// Whether this hook may run concurrently with its priority-group peers.
    pub parallel: bool,
    /// Force the hook to run without a concurrent peer (see
    /// [`Hook::serial_group`]; equivalent to joining [`SHARED_SERIAL_GROUP`]).
    pub require_serial: bool,
    /// The mutual-exclusion set this hook belongs to, if any.
    ///
    /// Hooks in a priority group run concurrently. Two hooks naming the **same**
    /// set never overlap; hooks in different sets — or in none — are unaffected.
    /// This is the model for a shared resource poly does not own: every cargo
    /// subcommand contends on cargo's package-cache and build-directory locks,
    /// so [`CARGO_SERIAL_GROUP`] makes that queue explicit (and each member's
    /// time budget start when it actually starts) instead of leaving four
    /// processes to block invisibly inside cargo.
    ///
    /// `None` with [`Hook::require_serial`] (or `parallel = false`) set means
    /// the shared set, [`SHARED_SERIAL_GROUP`].
    pub serial_group: Option<String>,
    /// When this hook fails, abort the remaining (higher-priority) groups.
    pub fail_fast: bool,
    /// When the hook modifies files and exits 0, `git add` the matched files
    /// and continue (only a non-zero exit fails the stage).
    pub stage_fixed: bool,
    /// Run even when no files match the filter.
    pub always_run: bool,
    /// Append the matched files to the invocation.
    pub pass_filenames: bool,
    /// Message printed when the hook fails.
    pub fail_text: Option<String>,
    /// Tier-1 result-cache policy (default [`HookCache::Disabled`]).
    pub cache: HookCache,
    /// Opt into tier-2 sccache env injection (`RUSTC_WRAPPER`, …). Only honoured
    /// when the run carries [`HookRunRequest::sccache`]; default `false`.
    pub compiler: bool,
    /// Whole-workspace hook: it compiles or analyses the entire project (e.g.
    /// `cargo clippy`, a type checker) rather than the per-file set.
    ///
    /// This is a *shape* flag, not an isolation flag: it decides whether the
    /// hook receives filenames, not which tree it runs in. Under a staged run
    /// ([`HookRunRequest::work_root`]) **every** hook runs from the snapshot,
    /// because a commit gate that judged two hooks against two different trees
    /// would be reporting on bytes that are not the ones being committed.
    pub workspace: bool,
    /// Applicability probe for **this hook only** (`sh -c`).
    ///
    /// Exit 0 runs the hook; non-zero (or a launch failure) marks it
    /// [`SkipReason::HookPrecondition`] — a visible skip that does **not** fail
    /// the stage, because a precondition answers "does this check apply here?".
    /// It is evaluated in the same tree and working directory the hook itself
    /// would run in (the staged snapshot for a [`Hook::workspace`] hook under
    /// isolation, the worktree otherwise), so a prerequisite that is satisfiable
    /// in the worktree but not in the staged tree is caught where it matters.
    pub precondition: Option<String>,
    /// Setup commands for **this hook only**, run sequentially before it.
    ///
    /// The first failure marks this hook — and only this hook —
    /// [`HookStatus::Unknown`]: its setup did not complete, so its verdict is
    /// unknown and the stage fails. Sibling hooks are unaffected. Like
    /// [`Hook::precondition`], these run in the hook's own execution root.
    pub before: Vec<String>,
    /// How long this hook may run before poly kills it and reports
    /// [`HookStatus::TimedOut`].
    ///
    /// [`HookTimeout::Default`] — the default — means "use the shape-derived
    /// default" ([`crate::timeout::budget_for`]): a long budget for a per-file
    /// hook, a far longer one for a [`Hook::workspace`] hook, since a cold
    /// `cargo clippy` legitimately runs for many minutes. The budget applies to
    /// each spawned process, so an `ARG_MAX`-batched hook gets it per batch.
    ///
    /// Lowered from the `poly.toml` job's `timeout` key, and outranked by the
    /// environment override — see [`crate::timeout`] for the whole chain.
    pub timeout: HookTimeout,
    /// Exclude this hook from the whole-project phase of `poly lint` while
    /// keeping it in git-hook runs. `poly lint`'s workspace phase drops every
    /// hook with this set (default `false` — participate in lint), so a tool can
    /// gate commits without also compiling the tree on every `poly lint` (e.g. a
    /// CI `validate` job with a plain checkout that cannot build the workspace).
    pub skip_in_lint: bool,
}

impl Hook {
    /// Create a hook that runs a shell command line, with sensible defaults
    /// (`parallel`, `pass_filenames`).
    #[must_use]
    pub fn run(id: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            command: HookCommand::Run(command.into()),
            parallel: true,
            pass_filenames: true,
            ..Self::default()
        }
    }

    /// Whether this hook must run on its own (forces a serial group).
    #[must_use]
    pub fn is_serial(&self) -> bool {
        self.require_serial || !self.parallel
    }

    /// Whether this hook belongs to the cargo exclusion set — i.e. whether
    /// running it means running `cargo`.
    ///
    /// The single classifier for that question. Membership is decided during
    /// lowering, where the command line is still understood (an explicit
    /// `serial = "cargo"`, or a `run` line whose program is `cargo` /
    /// `cargo-*`), so everything downstream — the scheduler, and the
    /// package-cache wait in [`crate::cargo_lock`] — reads one answer instead of
    /// re-deriving it from an argv it cannot parse.
    #[must_use]
    pub fn is_cargo(&self) -> bool {
        self.serial_group.as_deref() == Some(CARGO_SERIAL_GROUP)
    }
}

/// The per-stage execution unit: precondition → before → hooks → after.
#[derive(Debug, Clone, Default)]
pub struct StageSpec {
    /// The git stage.
    pub stage: Stage,
    /// Stage-wide applicability probe (`sh -c`); non-zero / missing → the whole
    /// stage is skipped and every hook is reported
    /// [`SkipReason::StagePrecondition`].
    ///
    /// Evaluated in the repository worktree ([`HookRunRequest::root`]), because
    /// it is not tied to any one hook and so has no execution root of its own.
    /// A prerequisite that only holds in the worktree therefore belongs on
    /// [`Hook::precondition`], not here.
    pub precondition: Option<String>,
    /// Stage-wide setup commands run sequentially before the hooks; the first
    /// failure aborts the stage and every hook is reported
    /// [`HookStatus::Unknown`]. Also evaluated in the worktree.
    pub before: Vec<String>,
    /// Teardown commands run after the hooks succeed; failure aborts.
    pub after: Vec<String>,
    /// The hooks to run (rayon-parallelised within priority groups).
    pub hooks: Vec<Hook>,
}

/// Resolved tier-2 sccache settings for a hook run.
///
/// `poly-hooks` must not depend on `poly-config`, so this is the runner-local
/// projection of the `[cache.sccache]` table: when a [`HookRunRequest`] carries
/// `Some(SccacheSettings)`, the runner starts the shared sccache server once per
/// process and injects `RUSTC_WRAPPER` / `SCCACHE_DIR` / `SCCACHE_CACHE_SIZE`
/// into every hook whose [`Hook::compiler`] flag is set.
#[derive(Debug, Clone, Default)]
pub struct SccacheSettings {
    /// Resolved `sccache` binary name or path (default `"sccache"`).
    pub bin: String,
    /// Optional `SCCACHE_DIR` storage directory.
    pub dir: Option<PathBuf>,
    /// Optional `SCCACHE_CACHE_SIZE` budget string (e.g. `"10G"`).
    pub max_size: Option<String>,
}

/// A request to run one or more stages.
#[derive(Debug, Clone, Default)]
pub struct HookRunRequest {
    /// Repository root; per-file hooks run with this as their working directory,
    /// and all git plumbing (staged files, re-staging fixes) targets it.
    pub root: PathBuf,
    /// Staged-content snapshot root — the tree this run validates.
    ///
    /// When `Some`, **every** hook runs from here: a non-destructive copy of the
    /// staged index (see [`crate::snapshot`]) holding exactly the bytes the
    /// commit would capture, with unstaged worktree edits and untracked files
    /// absent. This is what the git-hook / commit-gate path passes, because a
    /// gate must judge the index, not the worktree.
    ///
    /// `None` means the run is *about* the worktree — a manual `poly hooks run`,
    /// `--all-files`, a non-index stage, `poly lint`'s whole-project phase — and
    /// every hook runs from [`Self::root`].
    ///
    /// The two are never mixed within a run: [`HookOutcome::validated`] records
    /// the tree each hook was judged against so the report can say which bytes
    /// were checked.
    pub work_root: Option<PathBuf>,
    /// Candidate file universe (paths relative to `root`), filtered per hook.
    pub files: Vec<PathBuf>,
    /// Commit-message file path (for `commit-msg` / `prepare-commit-msg`).
    pub message_file: Option<PathBuf>,
    /// Stages to run, in order.
    pub stages: Vec<StageSpec>,
    /// Explicit concurrency override (`-j`); `None` → env / CPU count.
    pub concurrency: Option<usize>,
    /// Tier-1 result cache; `None` disables hook result caching for this run.
    ///
    /// [`ResultCache`] is `Send + Sync`, so the shared handle is borrowed
    /// directly inside the rayon pool — no `Arc` wrapper is needed.
    pub cache: Option<ResultCache>,
    /// Tier-2 sccache settings; `None` disables sccache env injection for this
    /// run (compiler hooks then run with the inherited environment).
    pub sccache: Option<SccacheSettings>,
    /// Emit live per-hook progress to stderr as each hook starts and finishes.
    ///
    /// Off by default (deterministic, quiet). The CLI enables it when stderr is
    /// a terminal so a long-running hook (`cargo clippy`, `cargo test`, …) is
    /// visibly *running* instead of looking like the commit has hung — the
    /// captured report is still rendered to stdout once the run completes.
    pub progress: bool,
}

/// The result of running all requested stages.
#[derive(Debug, Default)]
pub struct HookRunOutcome {
    /// Per-stage outcomes, in request order.
    pub stages: Vec<StageOutcome>,
}

impl HookRunOutcome {
    /// `true` when every stage ran (or was skipped) and no hook failed or was
    /// left without a verdict.
    #[must_use]
    pub fn success(&self) -> bool {
        self.stages.iter().all(StageOutcome::success)
    }

    /// How many hooks actually produced a pass/fail verdict.
    ///
    /// Skipped hooks (no matching files, or a precondition that declared them
    /// inapplicable) and hooks whose setup failed produced no verdict and are
    /// not counted.
    #[must_use]
    pub fn verdict_count(&self) -> usize {
        self.stages.iter().map(StageOutcome::verdict_count).sum()
    }

    /// How many configured hooks were withheld because a `precondition` — the
    /// stage's or their own — declared them inapplicable.
    #[must_use]
    pub fn precondition_skipped_count(&self) -> usize {
        self.stages.iter().map(StageOutcome::precondition_skipped_count).sum()
    }

    /// `true` when the run finished without validating anything: a
    /// `precondition` withheld at least one configured hook and **no** hook
    /// anywhere produced a verdict.
    ///
    /// This is the machine-readable form of "the gate reported success but
    /// checked nothing". A hook skipped for having no matching files does *not*
    /// count — "no relevant files changed" is a complete, correct verdict — so
    /// this never fires on an ordinary commit that misses every filter.
    ///
    /// The CLI maps it to a distinct exit code so a CI job reading only the exit
    /// status can tell "validated and clean" from "validated nothing".
    #[must_use]
    pub fn validated_nothing(&self) -> bool {
        self.precondition_skipped_count() > 0 && self.verdict_count() == 0
    }
}

/// The outcome of one stage.
#[derive(Debug)]
pub struct StageOutcome {
    /// The stage that ran.
    pub stage: Stage,
    /// Whether the stage ran, was skipped, or was aborted.
    pub status: StageStatus,
    /// `before` step outcomes, in order.
    pub before: Vec<StepOutcome>,
    /// Hook outcomes, in hook (position) order — deterministic.
    pub hooks: Vec<HookOutcome>,
    /// `after` step outcomes, in order.
    pub after: Vec<StepOutcome>,
}

impl StageOutcome {
    /// `true` when the stage was not aborted and no hook or step failed.
    ///
    /// A [`StageStatus::Skipped`] stage is a success: a `precondition` answers
    /// "does this apply here?", and "no" is a legitimate answer, not a fault.
    /// It is *not* silent, though — every withheld hook is listed in
    /// [`Self::hooks`] with its reason, and
    /// [`HookRunOutcome::validated_nothing`] gives a machine consumer the
    /// signal that nothing was checked.
    #[must_use]
    pub fn success(&self) -> bool {
        if matches!(self.status, StageStatus::Aborted(_)) {
            return false;
        }
        self.before.iter().all(|s| !s.status.is_failure())
            && self.after.iter().all(|s| !s.status.is_failure())
            && self.hooks.iter().all(|h| !h.status.is_failure())
    }

    /// How many of this stage's hooks produced a pass/fail verdict.
    #[must_use]
    pub fn verdict_count(&self) -> usize {
        self.hooks.iter().filter(|hook| hook.status.is_verdict()).count()
    }

    /// How many of this stage's hooks a `precondition` withheld.
    #[must_use]
    pub fn precondition_skipped_count(&self) -> usize {
        self.hooks
            .iter()
            .filter(|hook| matches!(&hook.status, HookStatus::Skipped(reason) if reason.is_precondition()))
            .count()
    }
}

/// Whether a stage ran, was skipped by its precondition, or aborted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StageStatus {
    /// The stage's hooks were executed.
    Ran,
    /// The precondition failed; the stage was skipped (not an error). Every
    /// configured hook is still listed, marked
    /// [`SkipReason::StagePrecondition`].
    Skipped(String),
    /// A `before`/`after` step failed; the stage was aborted. Every configured
    /// hook is still listed, marked [`HookStatus::Unknown`].
    Aborted(String),
}

/// The outcome of a single `before`/`after` step.
#[derive(Debug)]
pub struct StepOutcome {
    /// The shell command line that ran.
    pub command: String,
    /// Pass/fail status.
    pub status: HookStatus,
    /// Captured combined stdout+stderr.
    pub output: Vec<u8>,
}

/// The outcome of a single hook.
#[derive(Debug)]
pub struct HookOutcome {
    /// The hook's id.
    pub id: String,
    /// The hook's position within the stage (drives deterministic ordering).
    pub position: usize,
    /// Pass/fail/skip status.
    pub status: HookStatus,
    /// This hook's own `before` step outcomes, in order. Empty when the hook
    /// declares no `before`, or when it never reached them.
    pub before: Vec<StepOutcome>,
    /// Whether the hook modified files that were then re-staged (`stage_fixed`).
    pub files_modified: bool,
    /// Captured combined stdout+stderr, concatenated across `ARG_MAX` batches.
    pub output: Vec<u8>,
    /// Wall-clock execution time.
    pub duration: Duration,
    /// Whether this outcome was served from the tier-1 result cache (the hook
    /// body was not executed).
    pub cached: bool,
    /// The tree this hook's verdict was computed against.
    ///
    /// Recorded — and rendered — because a gate that does not say which bytes it
    /// read cannot be trusted to have read the right ones.
    pub validated: ValidatedTree,
}

/// Which tree a hook was evaluated against.
///
/// A commit gate and a manual whole-tree run answer different questions, and the
/// answers can differ: a file can be valid in the index and broken in the
/// worktree, or the reverse. Naming the tree in the outcome is what keeps the
/// two apart in the report instead of leaving the reader to assume.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ValidatedTree {
    /// The live working tree, including unstaged edits and untracked files.
    #[default]
    Worktree,
    /// The staged-content snapshot — byte-for-byte what a commit would capture.
    StagedIndex,
}

impl fmt::Display for ValidatedTree {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Worktree => formatter.write_str("worktree"),
            Self::StagedIndex => formatter.write_str("staged content"),
        }
    }
}

/// Pass/fail/skip status shared by hooks and steps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookStatus {
    /// Exited 0.
    Passed,
    /// Exited non-zero.
    Failed {
        /// The process exit code, if available.
        code: Option<i32>,
    },
    /// Deliberately not run, and that is fine: the hook does not apply here.
    /// Not a failure.
    Skipped(SkipReason),
    /// Not run because the setup it depends on failed, so **whatever this hook
    /// checks was not checked and its verdict is unknown**.
    ///
    /// Distinct from [`Self::Skipped`]: a skip says "does not apply", this says
    /// "should have applied, could not tell". Counts as a failure, so a run
    /// containing one never reports success.
    Unknown(UnknownReason),
    /// **poly killed the hook**: it was still running when its time budget
    /// elapsed, so it never reported anything and its verdict is unknown.
    ///
    /// Deliberately distinct from [`Self::Failed`]. "this tool says your code is
    /// wrong" and "poly stopped this tool after N seconds" call for different
    /// actions — fix the code versus raise the budget or fix the wedged tool —
    /// and collapsing them would put the reader back where the hang left them.
    /// Counts as a failure: a hook that was killed checked nothing, and a run
    /// that reported success on that basis would be a false pass.
    TimedOut(TimeoutReason),
    /// The hook exited 0 after fixing staged content, but the fix could **not**
    /// be carried into the worktree — for one of several distinct reasons, each
    /// carried per path in [`WithheldFix::reason`].
    ///
    /// The reasons are *not* interchangeable. "your unstaged work would have
    /// been destroyed" tells the author to stage or stash; "poly refused to
    /// write through this path" tells them a symlink or an escaping path is in
    /// the tree, which is a security finding and no amount of staging will make
    /// it land. Reporting one as the other sends the reader to fix something
    /// that is not broken — see [`WithheldReason`].
    ///
    /// Counts as a failure so the commit is blocked. The alternatives are worse:
    /// overwriting the author's unstaged work, staging hunks they deliberately
    /// left out, or committing the unfixed staged bytes while reporting a pass.
    /// Blocking hands the decision back to the author with nothing lost.
    FixWithheld(Vec<WithheldFix>),
    /// Failed to launch (binary not found, etc.).
    Error(String),
}

impl HookStatus {
    /// `true` for statuses that must fail the run: an explicit non-zero exit, a
    /// launch error, a hook whose verdict could not be determined, a hook poly
    /// killed for overrunning its budget, or a fix that could not be applied
    /// without losing work.
    #[must_use]
    pub fn is_failure(&self) -> bool {
        matches!(
            self,
            Self::Failed { .. } | Self::Error(_) | Self::Unknown(_) | Self::TimedOut(_) | Self::FixWithheld(_)
        )
    }

    /// `true` when the hook actually ran and produced a pass/fail answer.
    ///
    /// [`Self::FixWithheld`] counts: the hook ran and judged the staged content,
    /// and its answer was "this needed fixing". Only the delivery of the fix
    /// failed. [`Self::TimedOut`] does **not**: a killed hook was interrupted
    /// mid-check, so it answered nothing — same as [`Self::Unknown`].
    #[must_use]
    pub fn is_verdict(&self) -> bool {
        matches!(
            self,
            Self::Passed | Self::Failed { .. } | Self::Error(_) | Self::FixWithheld(_)
        )
    }
}

/// One path a hook's fix could not be written to, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WithheldFix {
    /// The repo-relative path the fix was meant for.
    pub path: PathBuf,
    /// Why writing it was refused.
    pub reason: WithheldReason,
}

impl WithheldFix {
    /// A withheld fix for `path`.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, reason: WithheldReason) -> Self {
        Self {
            path: path.into(),
            reason,
        }
    }
}

/// Why a fix computed from staged content was not written into the worktree.
///
/// Each variant sends the reader somewhere different, so they are kept apart
/// rather than collapsed into one message. Two of them —
/// [`Self::WorktreeIsSymlink`] and [`Self::PathEscapesRepository`] — are
/// **security refusals**: the destination is not a file poly may write, and the
/// write was declined rather than followed. Telling that author to "stage your
/// changes" would be actively misleading, since staging changes nothing about a
/// symlink pointing out of the repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WithheldReason {
    /// The worktree copy differs from the index, so the author is holding
    /// unstaged work the staged-content fix never saw. Writing it would destroy
    /// that work; staging it would commit hunks they left out.
    UnstagedChanges,
    /// The worktree entry is a **symlink**. Its target is arbitrary — including
    /// an absolute path outside the repository — so writing the fix through it
    /// would be an arbitrary file write. Refused, never followed.
    WorktreeIsSymlink,
    /// The index path leaves the repository once joined onto the worktree root
    /// (`..`, an absolute component). Refused rather than trusted.
    PathEscapesRepository,
    /// The worktree entry is neither a regular file nor a symlink — a
    /// directory, a device, or nothing at all — so there is no file to write.
    WorktreeNotRegularFile,
    /// The **fixed copy** in poly's staged snapshot could not be read as a
    /// regular file, so there was nothing to carry across. Nothing about the
    /// author's worktree is implicated.
    SnapshotUnreadable,
}

impl WithheldReason {
    /// `true` when poly refused the write to avoid touching something it must
    /// not — as opposed to declining so it would not clobber the author's work.
    ///
    /// The report leads with this, because a symlink or escaping path in a
    /// tracked tree is a finding in its own right and no user action on the
    /// index will change the outcome.
    #[must_use]
    pub fn is_security_refusal(&self) -> bool {
        matches!(self, Self::WorktreeIsSymlink | Self::PathEscapesRepository)
    }
}

impl fmt::Display for WithheldReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnstagedChanges => formatter.write_str("the worktree copy has unstaged changes the fix never saw"),
            Self::WorktreeIsSymlink => {
                formatter.write_str("the worktree entry is a symlink; poly refused to write through it")
            }
            Self::PathEscapesRepository => {
                formatter.write_str("the path leaves the repository; poly refused to write outside it")
            }
            Self::WorktreeNotRegularFile => formatter.write_str("the worktree entry is not a regular file"),
            Self::SnapshotUnreadable => {
                formatter.write_str("the fixed copy in poly's staged snapshot could not be read")
            }
        }
    }
}

/// Why a hook was skipped — none of these are faults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// No files matched the hook's filter and the hook is not `always_run`.
    NoFiles,
    /// The **stage's** `precondition` declared the whole stage inapplicable;
    /// this hook was withheld along with every other hook in the stage.
    StagePrecondition(String),
    /// The **hook's own** `precondition` declared this hook inapplicable. Its
    /// siblings are unaffected.
    HookPrecondition(String),
}

impl SkipReason {
    /// `true` when a `precondition` withheld the hook (as opposed to it simply
    /// having no files to work on).
    #[must_use]
    pub fn is_precondition(&self) -> bool {
        matches!(self, Self::StagePrecondition(_) | Self::HookPrecondition(_))
    }
}

impl fmt::Display for SkipReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoFiles => formatter.write_str("no matching files"),
            Self::StagePrecondition(command) => {
                write!(formatter, "stage precondition not met: {command}")
            }
            Self::HookPrecondition(command) => write!(formatter, "precondition not met: {command}"),
        }
    }
}

/// Whose setup failed, leaving a hook without a verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupScope {
    /// The stage's `before` list — every hook in the stage is affected.
    Stage,
    /// The hook's own `before` list — only this hook is affected.
    Hook,
}

impl fmt::Display for SetupScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stage => formatter.write_str("stage setup"),
            Self::Hook => formatter.write_str("setup"),
        }
    }
}

/// Why a hook's verdict is unknown: the `before` step naming `command` failed
/// in `root`, so the hook never executed.
///
/// `root` is recorded because it is the whole diagnosis in the case that
/// motivated this type: a prerequisite can be satisfiable in the worktree and
/// permanently unsatisfiable in the staged snapshot a `workspace` hook runs in
/// (a `.gitignore`d `gradle-wrapper.jar`, say). Reporting the directory the
/// command actually ran in stops the reader concluding "it works from my
/// worktree, so the hook works".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownReason {
    /// Whether the stage's setup or the hook's own setup failed.
    pub scope: SetupScope,
    /// The `before` command line that failed.
    pub command: String,
    /// The directory that command ran in — the tree the hook would have been
    /// evaluated against.
    pub root: PathBuf,
}

/// Why a hook has no verdict: poly killed it after `elapsed` because it
/// overran its `limit`.
///
/// Both numbers are recorded because the reader's next action depends on the
/// gap between them: a hook killed at 600.0s having run 600.1s is a budget that
/// is too tight for a legitimately slow tool, while one killed after a budget it
/// blew past by orders of magnitude is a wedged tool. Neither is the same fact
/// as "this tool found a problem".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeoutReason {
    /// Which of the hook's processes poly killed.
    pub phase: TimedOutPhase,
    /// The budget the hook was given.
    pub limit: Duration,
    /// How long it actually ran before poly killed it.
    pub elapsed: Duration,
}

impl TimeoutReason {
    /// A killed hook body, or a killed `before` / `after` step.
    #[must_use]
    pub const fn command(limit: Duration, elapsed: Duration) -> Self {
        Self {
            phase: TimedOutPhase::Command,
            limit,
            elapsed,
        }
    }

    /// A killed `precondition` probe — the hook itself never started.
    #[must_use]
    pub const fn precondition(limit: Duration, elapsed: Duration) -> Self {
        Self {
            phase: TimedOutPhase::Precondition,
            limit,
            elapsed,
        }
    }
}

/// Which process poly killed, so the reader is not left to assume it was the
/// tool itself.
///
/// "the hook ran 60s and was killed" and "the hook never ran because its
/// one-line probe hung" send the reader to different files, and the elapsed
/// time alone cannot tell them apart.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TimedOutPhase {
    /// The hook's own command, or a `before` / `after` step.
    #[default]
    Command,
    /// The applicability probe that decides whether the hook runs at all.
    Precondition,
}

impl fmt::Display for TimeoutReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let subject = match self.phase {
            TimedOutPhase::Command => "timed out",
            TimedOutPhase::Precondition => "precondition timed out",
        };
        write!(
            formatter,
            "{subject}: poly killed it after {}, limit {}",
            crate::reporter::format_duration(self.elapsed),
            crate::reporter::format_duration(self.limit)
        )
    }
}

impl fmt::Display for UnknownReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} failed in {}: {}",
            self.scope,
            self.root.display(),
            self.command
        )
    }
}
