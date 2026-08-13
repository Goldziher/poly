//! The native rayon hook runner.
//!
//! [`run`] is the public entry point: it sizes a dedicated rayon pool to the
//! effective concurrency, `install`s it, and executes each requested stage.
//!
//! Per-stage order is **precondition → before → hooks → after**:
//!
//! - `precondition` — a `sh -c` applicability probe; non-zero / launch failure
//!   **skips** the stage. Not a failure — but every configured hook is still
//!   listed as [`SkipReason::StagePrecondition`], and
//!   [`HookRunOutcome::validated_nothing`] reports that the run checked nothing.
//! - `before` — sequential setup commands; the first failure **aborts** the
//!   stage. Every configured hook is listed as [`HookStatus::Unknown`] — its
//!   verdict could not be determined, which fails the run.
//! - hooks — grouped by `priority` (lower first). Groups run sequentially; the
//!   hooks within a group run via rayon `par_iter` (unless any member forces a
//!   serial group). Each hook's `ARG_MAX` file batches also run via `par_iter`.
//!   Per-hook output is captured into its own buffer (no interleaving) and the
//!   final hook list is sorted by position for deterministic rendering.
//! - `after` — sequential teardown, only when no hook failed; aborts on
//!   non-zero.
//!
//! A hook may carry its **own** `precondition`/`before` ([`Hook::precondition`],
//! [`Hook::before`]). These are the scoped form and the one to prefer: they are
//! evaluated in the run's execution root — the same tree the hook itself runs in
//! — and a failure contains itself to that hook, so its siblings still report
//! verdicts. The stage-wide forms above run in the worktree and withhold
//! everything.
//!
//! # Which tree a run validates
//!
//! Every hook in a run is evaluated against **one** tree, named by
//! [`execution_root`]: the staged snapshot when the request carries a
//! [`HookRunRequest::work_root`] (the git-hook / commit-gate path), the live
//! worktree otherwise (`--all-files`, a manual run, `poly lint`). This is not a
//! per-hook property. A commit gate whose per-file hooks read the worktree while
//! its whole-workspace hooks read the index reports "passed" about two different
//! sets of bytes, and can pass a commit whose staged content it never saw. The
//! tree is recorded on every [`HookOutcome::validated`] and rendered in the
//! report, so the answer is never left to assumption.
//!
//! A hook that does not run is **never** omitted from the outcome: it is listed
//! with the reason it did not run, since a check that vanishes from the report is
//! how "nothing ran" becomes invisible.
//!
//! `fail_fast` is enforced at the sequential group boundary: when a failed hook
//! has `fail_fast` set, the remaining (higher-priority) groups are skipped.
//! `stage_fixed` is handled at the same boundary: a hook that exited 0 and
//! rewrote its matched files has those files `git add`ed (see
//! [`fixes::land_group_rewrites`] for what that means under a staged run), and
//! execution continues.
//!
//! Submodules: [`exec`] builds and runs subprocesses, [`cache_key`] derives the
//! tier-1 result-cache key, [`prepare`] resolves per-hook file sets and priority
//! groups, [`fixes`] detects rewritten files and lands their fixes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use indicatif::ProgressBar;
use poly_cache::{CacheKey, Namespace};
use rayon::prelude::*;
use tracing::warn;

use crate::model::{
    Hook, HookCache, HookOutcome, HookRunOutcome, HookRunRequest, HookStatus, SccacheSettings, SetupScope, SkipReason,
    StageOutcome, StageSpec, StageStatus, StepOutcome, UnknownReason, ValidatedTree,
};
use crate::reporter::{HookBar, ProgressUi};

/// The environment for a stage-level `precondition`/`before`: none of its own.
/// Stage steps are not tied to a hook, so they inherit the process environment
/// unchanged, while a hook's own steps get that hook's declared `env`.
static EMPTY_ENV: BTreeMap<String, String> = BTreeMap::new();

mod cache_key;
mod exec;
mod fixes;
mod prepare;

use self::fixes::Fingerprints;
use self::prepare::Prepared;

/// The tree every hook in `request` is evaluated against, and the root it runs
/// from.
///
/// A run is staged-scoped or worktree-scoped as a whole — never a mix. Two
/// hooks in one commit gate reading two different trees is precisely the
/// failure this collapses: the report would say "passed" without saying *what*
/// passed.
fn execution_root(request: &HookRunRequest) -> (&Path, ValidatedTree) {
    match request.work_root.as_deref() {
        Some(snapshot) => (snapshot, ValidatedTree::StagedIndex),
        None => (request.root.as_path(), ValidatedTree::Worktree),
    }
}

/// Everything about *where* a run's hooks execute — shared by every hook in the
/// run, resolved once.
struct ExecContext<'a> {
    /// The directory hooks run from: the staged snapshot or the worktree.
    root: &'a Path,
    /// The tree that root represents, recorded on every outcome.
    tree: ValidatedTree,
    /// Tier-2 sccache settings, when the run carries them.
    sccache: Option<&'a SccacheSettings>,
    /// `CARGO_TARGET_DIR` override. Under a staged run cargo is pointed back at
    /// the repository's own `target/` so the snapshot does not force a cold
    /// rebuild (ADR 0019); in a worktree run cargo already has it.
    cargo_target_dir: Option<PathBuf>,
}

impl<'a> ExecContext<'a> {
    fn new(request: &'a HookRunRequest) -> Self {
        let (root, tree) = execution_root(request);
        Self {
            root,
            tree,
            sccache: request.sccache.as_ref(),
            cargo_target_dir: matches!(tree, ValidatedTree::StagedIndex).then(|| request.root.join("target")),
        }
    }
}

/// One hook's execution, plus what the sequential boundary needs from it.
struct HookRun {
    /// The hook's outcome, before `stage_fixed` write-back is applied.
    outcome: HookOutcome,
    /// Cache key to store the outcome under — `Some` only on a cacheable miss.
    store_key: Option<CacheKey>,
    /// Files the hook rewrote inside its execution root.
    modified: Vec<PathBuf>,
}

/// Run the requested stages, returning a per-stage outcome.
///
/// Builds a dedicated rayon pool sized to the effective concurrency (the
/// request's `-j` override, else `PREK_MAX_CONCURRENCY` / CPU count) and runs
/// the whole pipeline inside `pool.install`, so every nested `par_iter` uses
/// this pool.
///
/// # Errors
///
/// Returns `Err` if the rayon pool cannot be built or a git index operation
/// (used by `stage_fixed`) fails.
#[allow(clippy::needless_pass_by_value)]
pub fn run(request: HookRunRequest) -> anyhow::Result<HookRunOutcome> {
    let threads = crate::concurrency::effective_concurrency(request.concurrency);
    let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build()?;
    let ui = request.progress.then(ProgressUi::new);
    pool.install(|| run_all(&request, ui.as_ref()))
}

fn run_all(request: &HookRunRequest, ui: Option<&ProgressUi>) -> anyhow::Result<HookRunOutcome> {
    let mut stages = Vec::with_capacity(request.stages.len());
    for spec in &request.stages {
        stages.push(run_stage(request, spec, ui)?);
    }
    Ok(HookRunOutcome { stages })
}

fn run_stage(request: &HookRunRequest, spec: &StageSpec, ui: Option<&ProgressUi>) -> anyhow::Result<StageOutcome> {
    let (_, tree) = execution_root(request);
    if let Some(precondition) = &spec.precondition {
        match exec::run_precondition(
            &request.root,
            precondition,
            &EMPTY_ENV,
            crate::timeout::precondition_budget(),
        ) {
            exec::Probe::Passed => {}
            exec::Probe::Declined => {
                warn!(stage = %spec.stage, "precondition failed; skipping stage");
                return Ok(StageOutcome {
                    stage: spec.stage,
                    status: StageStatus::Skipped(format!("precondition failed: {precondition}")),
                    before: Vec::new(),
                    hooks: withheld_hooks(spec, tree, |_| {
                        HookStatus::Skipped(SkipReason::StagePrecondition(precondition.clone()))
                    }),
                    after: Vec::new(),
                });
            }
            // A killed probe is **not** a skip: it never answered, so whether
            // these hooks applied is unknown and the run must fail rather than
            // pass having validated nothing.
            exec::Probe::TimedOut(reason) => {
                warn!(stage = %spec.stage, "precondition timed out; aborting stage");
                return Ok(StageOutcome {
                    stage: spec.stage,
                    status: StageStatus::Aborted(format!("precondition timed out: {precondition}")),
                    before: Vec::new(),
                    hooks: withheld_hooks(spec, tree, |_| HookStatus::TimedOut(reason)),
                    after: Vec::new(),
                });
            }
        }
    }

    let mut before = Vec::new();
    for command in &spec.before {
        let step = exec::run_step(&request.root, command, &EMPTY_ENV, crate::timeout::step_budget());
        let failed = step.status.is_failure();
        let killed = matches!(step.status, HookStatus::TimedOut(_));
        before.push(step);
        if failed {
            return Ok(StageOutcome {
                stage: spec.stage,
                status: StageStatus::Aborted(abort_reason("before", killed, command)),
                before,
                hooks: withheld_hooks(spec, tree, |_| {
                    HookStatus::Unknown(UnknownReason {
                        scope: SetupScope::Stage,
                        command: command.clone(),
                        root: request.root.clone(),
                    })
                }),
                after: Vec::new(),
            });
        }
    }

    let prepared = prepare::prepare(request, spec);
    let (mut hooks, any_failed) = run_hooks(request, spec, &prepared, ui)?;
    hooks.sort_by_key(|hook| hook.position);

    let mut after = Vec::new();
    if !any_failed {
        for command in &spec.after {
            let step = exec::run_step(&request.root, command, &EMPTY_ENV, crate::timeout::step_budget());
            let failed = step.status.is_failure();
            let killed = matches!(step.status, HookStatus::TimedOut(_));
            after.push(step);
            if failed {
                return Ok(StageOutcome {
                    stage: spec.stage,
                    status: StageStatus::Aborted(abort_reason("after", killed, command)),
                    before,
                    hooks,
                    after,
                });
            }
        }
    }

    Ok(StageOutcome {
        stage: spec.stage,
        status: StageStatus::Ran,
        before,
        hooks,
        after,
    })
}

/// Why a stage aborted on a `before`/`after` step, keeping "poly killed it"
/// apart from "the command said no" — the two send the reader to different
/// places.
fn abort_reason(label: &str, killed: bool, command: &str) -> String {
    let verdict = if killed { "timed out" } else { "failed" };
    format!("{label} step {verdict}: {command}")
}

/// Run the stage's hooks in priority-group order, landing rewrites and applying
/// `fail_fast` at each sequential group boundary.
fn run_hooks(
    request: &HookRunRequest,
    spec: &StageSpec,
    prepared: &[Prepared],
    ui: Option<&ProgressUi>,
) -> anyhow::Result<(Vec<HookOutcome>, bool)> {
    let mut collected = Vec::with_capacity(spec.hooks.len());
    let mut any_failed = false;

    for group in prepare::group_by_priority(&spec.hooks) {
        let serial = group.iter().any(|&pos| spec.hooks[pos].is_serial());
        let mut runs = run_group(request, spec, prepared, &group, serial, ui);

        // Landing is a group-wide step: the hooks ran concurrently, so who
        // rewrote what has to be reconciled once, before any outcome is final.
        let stage_fixed: Vec<bool> = group.iter().map(|&pos| spec.hooks[pos].stage_fixed).collect();
        fixes::land_group_rewrites(&request.root, request.work_root.as_deref(), &stage_fixed, &mut runs)?;

        let mut abort = false;
        for (&pos, run) in group.iter().zip(runs) {
            let HookRun {
                outcome,
                store_key,
                mut modified,
            } = run;
            let hook = &spec.hooks[pos];

            // A `DeclaredInputs` hook depends on far more than its matched files,
            // so a second, coarser dirtiness probe covers the declared set. It
            // asks the worktree rather than the execution root, which under a
            // staged run only ever *suppresses* a store — never permits a wrong
            // one — so it stays as the conservative guard it has always been.
            if modified.is_empty() && store_key.is_some() {
                if let HookCache::DeclaredInputs(pattern) = &hook.cache {
                    let declared = cache_key::declared_input_files(&request.root, pattern)?;
                    modified = cache_key::modified_matched(&request.root, &declared)?;
                }
            }

            if let (Some(cache), Some(key)) = (request.cache.as_ref(), &store_key) {
                if matches!(outcome.status, HookStatus::Passed) && modified.is_empty() {
                    if let Err(error) = cache.put(Namespace::Hook, key, &outcome.output) {
                        warn!(hook = %hook.id, "failed to store hook result cache entry: {error:#}");
                    }
                }
            }

            if outcome.status.is_failure() {
                any_failed = true;
                if hook.fail_fast {
                    abort = true;
                }
            }
            collected.push(outcome);
        }
        if abort {
            break;
        }
    }

    Ok((collected, any_failed))
}

/// Run one priority group, returning each hook's [`HookRun`].
fn run_group(
    request: &HookRunRequest,
    spec: &StageSpec,
    prepared: &[Prepared],
    group: &[usize],
    serial: bool,
    ui: Option<&ProgressUi>,
) -> Vec<HookRun> {
    // Resolved once per group rather than per hook: the tree is a property of
    // the run, and every hook in the run shares it.
    let context = ExecContext::new(request);
    let (exec_root, tree) = (context.root, context.tree);

    let run_one = |&pos: &usize| -> HookRun {
        let hook = &spec.hooks[pos];
        let blank = |outcome| HookRun {
            outcome,
            store_key: None,
            modified: Vec::new(),
        };
        if let Some(reason) = &prepared[pos].skip {
            return blank(skipped_outcome(hook, pos, tree, reason.clone()));
        }
        let matched = &prepared[pos].matched;

        // A hook's own `precondition`/`before` must be evaluated in the tree the
        // hook itself runs in — a prerequisite checked against the wrong tree
        // points the diagnosis at the wrong place.
        let hook_dir = exec::hook_working_dir(hook, exec_root);

        // Applicability is decided before the cache is consulted: a hook that no
        // longer applies must not be served a stored "passed" from when it did.
        if let Some(precondition) = &hook.precondition {
            match exec::run_precondition(
                &hook_dir,
                precondition,
                &hook.env,
                crate::timeout::precondition_budget(),
            ) {
                exec::Probe::Passed => {}
                exec::Probe::Declined => {
                    let reason = SkipReason::HookPrecondition(precondition.clone());
                    return blank(skipped_outcome(hook, pos, tree, reason));
                }
                // Scoped, like every other per-hook failure: this hook has no
                // verdict, its siblings still report theirs.
                exec::Probe::TimedOut(reason) => {
                    return blank(HookOutcome {
                        status: HookStatus::TimedOut(reason),
                        ..blank_outcome(hook, pos, tree)
                    });
                }
            }
        }

        let key = request
            .cache
            .as_ref()
            .and_then(|_| cache_key::cache_key(&request.root, exec_root, hook, matched));

        if let (Some(cache), Some(key)) = (request.cache.as_ref(), key.as_ref()) {
            if let Some(output) = cache.get(Namespace::Hook, key) {
                return blank(cached_outcome(hook, pos, tree, output));
            }
        }

        // Setup runs only once the hook is known to need executing — a cache hit
        // means there is nothing to set up for.
        let (before, setup_failure) = run_hook_before(&hook_dir, hook);
        if let Some(status) = setup_failure {
            return blank(HookOutcome {
                status,
                before,
                ..blank_outcome(hook, pos, tree)
            });
        }

        // Fingerprint the inputs only when the answer is consumed: to re-stage a
        // fix, to decide whether a passing run may be cached, and — under a
        // staged run — to carry a rewrite of the snapshot back into the worktree
        // so it is not silently discarded on the next refresh.
        let watch_rewrites = hook.stage_fixed || key.is_some() || tree == ValidatedTree::StagedIndex;
        let watched = if watch_rewrites {
            Fingerprints::capture(exec_root, matched)
        } else {
            Fingerprints::none()
        };

        let refs: Vec<&Path> = matched.iter().map(AsRef::as_ref).collect();
        let hook_bar = ui.map(|ui| ui.start(&hook.id));
        let mut outcome = run_hook(&context, hook, pos, &refs, hook_bar.as_ref().map(HookBar::bar));
        outcome.before = before;
        if let (Some(ui), Some(bar)) = (ui, hook_bar.as_ref()) {
            ui.finish(bar, outcome.status.is_failure(), outcome.duration);
        }
        HookRun {
            modified: watched.modified(exec_root),
            outcome,
            store_key: key,
        }
    };

    if serial {
        group.iter().map(run_one).collect()
    } else {
        group.par_iter().map(run_one).collect()
    }
}

/// Run a hook's own `before` steps in `hook_dir`, stopping at the first failure.
///
/// Returns the step outcomes plus, on failure, the [`HookStatus::Unknown`] the
/// hook must carry. Containment lives here: the failure is scoped to this hook,
/// so its siblings still run and still report real verdicts.
fn run_hook_before(hook_dir: &Path, hook: &Hook) -> (Vec<StepOutcome>, Option<HookStatus>) {
    let mut steps = Vec::with_capacity(hook.before.len());
    for command in &hook.before {
        let step = exec::run_step(hook_dir, command, &hook.env, crate::timeout::step_budget());
        let failed = step.status.is_failure();
        steps.push(step);
        if failed {
            let status = HookStatus::Unknown(UnknownReason {
                scope: SetupScope::Hook,
                command: command.clone(),
                root: hook_dir.to_path_buf(),
            });
            return (steps, Some(status));
        }
    }
    (steps, None)
}

/// Execute a single hook over its matched files, splitting into `ARG_MAX` batches
/// run via `par_iter`. Passes only when every batch passes; output is
/// concatenated in batch order.
fn run_hook(
    context: &ExecContext<'_>,
    hook: &Hook,
    position: usize,
    matched: &[&Path],
    bar: Option<&ProgressBar>,
) -> HookOutcome {
    let start = Instant::now();
    let base_len = exec::base_arg_len(hook);
    let batches = crate::concurrency::partition_files(matched, base_len);
    // Resolved once per hook, not per batch: the budget is a property of the
    // hook, and each spawned batch process gets the whole of it.
    let budget = crate::timeout::budget_for(hook);

    let results: Vec<(HookStatus, Vec<u8>)> = batches
        .into_par_iter()
        .map(|batch| {
            let cmd = exec::build_command(
                hook,
                context.root,
                batch,
                context.sccache,
                context.cargo_target_dir.as_deref(),
            );
            exec::execute(cmd, bar, &hook.id, budget)
        })
        .collect();

    let mut output = Vec::new();
    let mut status = HookStatus::Passed;
    for (batch_status, batch_output) in results {
        output.extend_from_slice(&batch_output);
        if !status.is_failure() && batch_status.is_failure() {
            status = batch_status;
        }
    }

    HookOutcome {
        id: hook.id.clone(),
        position,
        status,
        before: Vec::new(),
        files_modified: false,
        output,
        duration: start.elapsed(),
        cached: false,
        validated: context.tree,
    }
}

/// Name every hook a stage never got to, so "nothing ran" is never invisible.
///
/// A stage that is skipped or aborted before its hooks execute still reports
/// each configured hook, with `status_for` supplying the reason. Without this a
/// zeroed stage rendered an empty hook list, and a check that validated nothing
/// was indistinguishable from a check that was never configured.
fn withheld_hooks(spec: &StageSpec, tree: ValidatedTree, status_for: impl Fn(&Hook) -> HookStatus) -> Vec<HookOutcome> {
    spec.hooks
        .iter()
        .enumerate()
        .map(|(position, hook)| HookOutcome {
            status: status_for(hook),
            ..blank_outcome(hook, position, tree)
        })
        .collect()
}

/// A zero-cost outcome shell for a hook that did not execute.
///
/// `tree` is recorded even here: it is the tree the hook *would* have been
/// judged against, which is what a reader needs to know when asking why a check
/// did not run.
fn blank_outcome(hook: &Hook, position: usize, tree: ValidatedTree) -> HookOutcome {
    HookOutcome {
        id: hook.id.clone(),
        position,
        status: HookStatus::Passed,
        before: Vec::new(),
        files_modified: false,
        output: Vec::new(),
        duration: Duration::ZERO,
        cached: false,
        validated: tree,
    }
}

fn skipped_outcome(hook: &Hook, position: usize, tree: ValidatedTree, reason: SkipReason) -> HookOutcome {
    HookOutcome {
        status: HookStatus::Skipped(reason),
        ..blank_outcome(hook, position, tree)
    }
}

/// Build the outcome for a hook served from the result cache: a passing,
/// zero-duration run carrying the stored output bytes.
fn cached_outcome(hook: &Hook, position: usize, tree: ValidatedTree, output: Vec<u8>) -> HookOutcome {
    HookOutcome {
        output,
        cached: true,
        ..blank_outcome(hook, position, tree)
    }
}
