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
use std::path::PathBuf;
use std::time::Duration;

use poly_cache::ResultCache;

use crate::filter::FilePattern;
use crate::identify::TagSet;
use crate::stage::Stage;

/// How a hook participates in tier-1 result caching.
///
/// The runner only ever stores an entry for a **passing, tree-clean** run, so a
/// cache hit always means "passed without modifying its inputs".
// `DeclaredInputs` carries a [`FilePattern`] (which wraps a compiled regex /
// glob set), and those are not `PartialEq`/`Eq`, so this enum cannot derive
// them — use [`HookCache::is_enabled`] / `matches!` instead of `==`.
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

/// One runnable unit within a stage — a single subprocess invocation.
///
/// Mirrors the poly.toml `Job` shape but carries only what the runner needs:
/// no globs-as-strings (patterns are pre-compiled into [`FilePattern`]), no
/// cache declaration (Workstream C), no `skip`/`only` guards (resolved during
/// lowering in B3).
// The flag set (parallel / require_serial / fail_fast / stage_fixed /
// always_run / pass_filenames) is the model's natural shape — each is an
// independent execution toggle mirrored from the poly.toml `Job` schema, not a
// state machine that would collapse into an enum.
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
    /// Force the hook (and thus its whole priority group) to run serially.
    pub require_serial: bool,
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
    /// `cargo clippy`, a type checker) rather than the per-file set. When the
    /// run carries a [`HookRunRequest::work_root`] staged snapshot, such a hook
    /// runs from there instead of the live worktree, isolating it to staged
    /// content. Per-file hooks (default `false`) are unaffected.
    pub workspace: bool,
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
}

/// The per-stage execution unit: precondition → before → hooks → after.
#[derive(Debug, Clone, Default)]
pub struct StageSpec {
    /// The git stage.
    pub stage: Stage,
    /// Guard command (`sh -c`); non-zero / missing → skip the stage.
    pub precondition: Option<String>,
    /// Setup commands run sequentially before the hooks; failure aborts.
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
    /// Staged-content snapshot root for whole-workspace hook isolation.
    ///
    /// When `Some`, a [`Hook::workspace`] hook runs from here — a non-destructive
    /// copy of the staged index (see [`crate::snapshot`]) — so it sees staged
    /// content only, never unstaged worktree edits or untracked files. `None`
    /// (e.g. `--all-files`, or a stage with no workspace hooks) runs every hook
    /// from `root` as before.
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
    /// `true` when every stage ran (or was skipped) and no hook failed.
    #[must_use]
    pub fn success(&self) -> bool {
        self.stages.iter().all(StageOutcome::success)
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
    #[must_use]
    pub fn success(&self) -> bool {
        if matches!(self.status, StageStatus::Aborted(_)) {
            return false;
        }
        self.before.iter().all(|s| !s.status.is_failure())
            && self.after.iter().all(|s| !s.status.is_failure())
            && self.hooks.iter().all(|h| !h.status.is_failure())
    }
}

/// Whether a stage ran, was skipped by its precondition, or aborted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StageStatus {
    /// The stage's hooks were executed.
    Ran,
    /// The precondition failed; the stage was skipped (not an error).
    Skipped(String),
    /// A `before`/`after` step failed; the stage was aborted.
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
    /// Whether the hook modified files that were then re-staged (`stage_fixed`).
    pub files_modified: bool,
    /// Captured combined stdout+stderr, concatenated across `ARG_MAX` batches.
    pub output: Vec<u8>,
    /// Wall-clock execution time.
    pub duration: Duration,
    /// Whether this outcome was served from the tier-1 result cache (the hook
    /// body was not executed).
    pub cached: bool,
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
    /// Not run (e.g. no matched files and not `always_run`).
    Skipped(SkipReason),
    /// Failed to launch (binary not found, etc.).
    Error(String),
}

impl HookStatus {
    /// `true` for [`HookStatus::Failed`] / [`HookStatus::Error`].
    #[must_use]
    pub fn is_failure(&self) -> bool {
        matches!(self, Self::Failed { .. } | Self::Error(_))
    }
}

/// Why a hook was skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// No files matched the hook's filter and the hook is not `always_run`.
    NoFiles,
}
