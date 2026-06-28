//! The native rayon hook runner.
//!
//! [`run`] is the public entry point: it sizes a dedicated rayon pool to the
//! effective concurrency, `install`s it, and executes each requested stage.
//!
//! Per-stage order is **precondition → before → hooks → after**:
//!
//! - `precondition` — a `sh -c` guard; non-zero / launch failure **skips** the
//!   stage (a warning, not an abort).
//! - `before` — sequential setup commands; the first failure **aborts** the
//!   stage.
//! - hooks — grouped by `priority` (lower first). Groups run sequentially; the
//!   hooks within a group run via rayon `par_iter` (unless any member forces a
//!   serial group). Each hook's `ARG_MAX` file batches also run via `par_iter`.
//!   Per-hook output is captured into its own buffer (no interleaving) and the
//!   final hook list is sorted by position for deterministic rendering.
//! - `after` — sequential teardown, only when no hook failed; aborts on
//!   non-zero.
//!
//! `fail_fast` is enforced at the sequential group boundary: when a failed hook
//! has `fail_fast` set, the remaining (higher-priority) groups are skipped.
//! `stage_fixed` is handled at the same boundary: a hook that exited 0 and
//! modified its matched files has those files `git add`ed, and execution
//! continues.

use std::path::Path;
use std::process::Stdio;
use std::time::Instant;

use rayon::prelude::*;
use tracing::warn;

use crate::filter::{FileTagCache, HookFileFilter};
use crate::git;
use crate::model::{
    Hook, HookCommand, HookOutcome, HookRunOutcome, HookRunRequest, HookStatus, SkipReason,
    StageOutcome, StageSpec, StageStatus, StepOutcome,
};
use crate::process::Cmd;
use crate::reporter::CaptureSink;
use crate::stage::RunInputMode;

#[cfg(not(windows))]
const SHELL: &str = "sh";
#[cfg(not(windows))]
const SHELL_ARG: &str = "-c";
#[cfg(windows)]
const SHELL: &str = "cmd";
#[cfg(windows)]
const SHELL_ARG: &str = "/C";

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
// The public B1 contract takes the request by value; the runner immediately
// borrows it for the pool closure, so by-value ownership is intentional.
#[allow(clippy::needless_pass_by_value)]
pub fn run(request: HookRunRequest) -> anyhow::Result<HookRunOutcome> {
    let threads = crate::concurrency::effective_concurrency(request.concurrency);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()?;
    pool.install(|| run_all(&request))
}

fn run_all(request: &HookRunRequest) -> anyhow::Result<HookRunOutcome> {
    let mut stages = Vec::with_capacity(request.stages.len());
    for spec in &request.stages {
        stages.push(run_stage(request, spec)?);
    }
    Ok(HookRunOutcome { stages })
}

/// A hook's resolved file set and skip decision, computed before execution.
struct Prepared {
    matched: Vec<std::path::PathBuf>,
    skip: Option<SkipReason>,
}

fn run_stage(request: &HookRunRequest, spec: &StageSpec) -> anyhow::Result<StageOutcome> {
    // 1. precondition guard.
    if let Some(precondition) = &spec.precondition {
        if !run_precondition(&request.root, precondition) {
            warn!(stage = %spec.stage, "precondition failed; skipping stage");
            return Ok(StageOutcome {
                stage: spec.stage,
                status: StageStatus::Skipped(format!("precondition failed: {precondition}")),
                before: Vec::new(),
                hooks: Vec::new(),
                after: Vec::new(),
            });
        }
    }

    // 2. before steps — abort on first failure.
    let mut before = Vec::new();
    for command in &spec.before {
        let step = run_step(&request.root, command);
        let failed = step.status.is_failure();
        before.push(step);
        if failed {
            return Ok(StageOutcome {
                stage: spec.stage,
                status: StageStatus::Aborted(format!("before step failed: {command}")),
                before,
                hooks: Vec::new(),
                after: Vec::new(),
            });
        }
    }

    // 3. hooks — priority groups, par_iter within a group.
    let prepared = prepare(request, spec);
    let (mut hooks, any_failed) = run_hooks(request, spec, &prepared)?;
    hooks.sort_by_key(|hook| hook.position);

    // 4. after steps — only when no hook failed.
    let mut after = Vec::new();
    if !any_failed {
        for command in &spec.after {
            let step = run_step(&request.root, command);
            let failed = step.status.is_failure();
            after.push(step);
            if failed {
                return Ok(StageOutcome {
                    stage: spec.stage,
                    status: StageStatus::Aborted(format!("after step failed: {command}")),
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

/// Run the stage's hooks in priority-group order, applying `stage_fixed`
/// re-staging and `fail_fast` at each sequential group boundary.
fn run_hooks(
    request: &HookRunRequest,
    spec: &StageSpec,
    prepared: &[Prepared],
) -> anyhow::Result<(Vec<HookOutcome>, bool)> {
    let mut collected = Vec::with_capacity(spec.hooks.len());
    let mut any_failed = false;

    for group in group_by_priority(&spec.hooks) {
        let serial = group.iter().any(|&pos| spec.hooks[pos].is_serial());
        let outcomes = run_group(request, spec, prepared, &group, serial);

        let mut abort = false;
        for (&pos, mut outcome) in group.iter().zip(outcomes) {
            let hook = &spec.hooks[pos];
            if matches!(outcome.status, HookStatus::Passed) && hook.stage_fixed {
                let modified = modified_matched(&request.root, &prepared[pos].matched)?;
                if !modified.is_empty() {
                    git::add(&request.root, &modified)?;
                    outcome.files_modified = true;
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

fn run_group(
    request: &HookRunRequest,
    spec: &StageSpec,
    prepared: &[Prepared],
    group: &[usize],
    serial: bool,
) -> Vec<HookOutcome> {
    let run_one = |&pos: &usize| -> HookOutcome {
        let hook = &spec.hooks[pos];
        if let Some(reason) = &prepared[pos].skip {
            return skipped_outcome(hook, pos, reason.clone());
        }
        let refs: Vec<&Path> = prepared[pos].matched.iter().map(AsRef::as_ref).collect();
        run_hook(&request.root, hook, pos, &refs)
    };

    if serial {
        group.iter().map(run_one).collect()
    } else {
        group.par_iter().map(run_one).collect()
    }
}

/// Execute a single hook over its matched files, splitting into `ARG_MAX` batches
/// run via `par_iter`. Passes only when every batch passes; output is
/// concatenated in batch order.
fn run_hook(root: &Path, hook: &Hook, position: usize, matched: &[&Path]) -> HookOutcome {
    let start = Instant::now();
    let base_len = base_arg_len(hook);
    let batches = crate::concurrency::partition_files(matched, base_len);

    let results: Vec<(HookStatus, Vec<u8>)> = batches
        .into_par_iter()
        .map(|batch| execute(build_command(hook, root, batch)))
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
        files_modified: false,
        output,
        duration: start.elapsed(),
    }
}

fn skipped_outcome(hook: &Hook, position: usize, reason: SkipReason) -> HookOutcome {
    HookOutcome {
        id: hook.id.clone(),
        position,
        status: HookStatus::Skipped(reason),
        files_modified: false,
        output: Vec::new(),
        duration: std::time::Duration::ZERO,
    }
}

// ── Command construction & execution ────────────────────────────────────────

fn build_command(hook: &Hook, root: &Path, files: &[&Path]) -> Cmd {
    let mut cmd = match &hook.command {
        HookCommand::Run(line) => shell_command(line, &hook.args, files, hook.pass_filenames),
        HookCommand::Script { path, runner } => {
            let mut cmd = match runner {
                Some(runner) => {
                    let mut cmd = Cmd::new(runner, hook.id.clone());
                    cmd.arg(path);
                    cmd
                }
                None => Cmd::new(path, hook.id.clone()),
            };
            cmd.args(&hook.args);
            if hook.pass_filenames {
                cmd.args(files.iter().map(|p| p.as_os_str()));
            }
            cmd
        }
    };
    cmd.current_dir(root);
    cmd.envs(hook.env.iter());
    cmd
}

#[cfg(not(windows))]
fn shell_command(line: &str, args: &[String], files: &[&Path], pass_filenames: bool) -> Cmd {
    // `sh -c '<line> "$@"' poly-hook <args> <files>` — args and matched files
    // become the positional parameters consumed by `"$@"`. `$0` is a label.
    let mut cmd = Cmd::new(SHELL, line.to_string());
    cmd.arg(SHELL_ARG)
        .arg(format!("{line} \"$@\""))
        .arg("poly-hook");
    cmd.args(args);
    if pass_filenames {
        cmd.args(files.iter().map(|p| p.as_os_str()));
    }
    cmd
}

#[cfg(windows)]
fn shell_command(line: &str, args: &[String], files: &[&Path], pass_filenames: bool) -> Cmd {
    // `cmd /C` has no `"$@"`, so join the command, args, and files into one line.
    let mut joined = line.to_string();
    for arg in args {
        joined.push(' ');
        joined.push_str(arg);
    }
    if pass_filenames {
        for file in files {
            joined.push(' ');
            joined.push_str(&file.to_string_lossy());
        }
    }
    let mut cmd = Cmd::new(SHELL, line.to_string());
    cmd.arg(SHELL_ARG).arg(joined);
    cmd
}

fn execute(mut cmd: Cmd) -> (HookStatus, Vec<u8>) {
    let mut sink = CaptureSink::default();
    cmd.check(false);
    match cmd.output_with_sink(&mut sink) {
        Ok(output) => {
            let status = if output.status.success() {
                HookStatus::Passed
            } else {
                HookStatus::Failed {
                    code: output.status.code(),
                }
            };
            (status, sink.into_bytes())
        }
        Err(error) => (HookStatus::Error(error.to_string()), sink.into_bytes()),
    }
}

fn run_step(root: &Path, command: &str) -> StepOutcome {
    let mut cmd = Cmd::new(SHELL, command.to_string());
    cmd.arg(SHELL_ARG).arg(command).current_dir(root);
    let (status, output) = execute(cmd);
    StepOutcome {
        command: command.to_string(),
        status,
        output,
    }
}

fn run_precondition(root: &Path, command: &str) -> bool {
    let mut cmd = Cmd::new(SHELL, command.to_string());
    cmd.arg(SHELL_ARG)
        .arg(command)
        .current_dir(root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .check(false);
    cmd.status().is_ok_and(|status| status.success())
}

// ── Preparation helpers ─────────────────────────────────────────────────────

fn prepare(request: &HookRunRequest, spec: &StageSpec) -> Vec<Prepared> {
    let all_paths: Vec<&Path> = request.files.iter().map(AsRef::as_ref).collect();
    let tag_cache = FileTagCache::from_paths(all_paths.iter().copied());
    spec.hooks
        .iter()
        .map(|hook| prepare_one(request, hook, &all_paths, &tag_cache))
        .collect()
}

fn prepare_one(
    request: &HookRunRequest,
    hook: &Hook,
    all_paths: &[&Path],
    tag_cache: &FileTagCache<'_>,
) -> Prepared {
    match RunInputMode::from(hook.stage) {
        RunInputMode::NoFiles => Prepared {
            matched: Vec::new(),
            skip: None,
        },
        RunInputMode::MessageFile => Prepared {
            matched: request.message_file.iter().cloned().collect(),
            skip: None,
        },
        RunInputMode::Files => {
            let filter = HookFileFilter::new(
                hook.files.as_ref(),
                hook.exclude.as_ref(),
                hook.types.as_ref(),
                hook.types_or.as_ref(),
                hook.exclude_types.as_ref(),
            );
            let has_tag_filter =
                hook.types.is_some() || hook.types_or.is_some() || hook.exclude_types.is_some();
            let matched: Vec<std::path::PathBuf> = all_paths
                .iter()
                .filter(|path| {
                    filter.matches_filename(path)
                        && (!has_tag_filter || filter.matches_tags(tag_cache.tags_for(path)))
                })
                .map(|path| path.to_path_buf())
                .collect();
            let skip = if matched.is_empty() && !hook.always_run {
                Some(SkipReason::NoFiles)
            } else {
                None
            };
            Prepared { matched, skip }
        }
    }
}

/// Group hook positions by `priority` (ascending), preserving original order
/// within a group.
fn group_by_priority(hooks: &[Hook]) -> Vec<Vec<usize>> {
    let mut order: Vec<usize> = (0..hooks.len()).collect();
    order.sort_by_key(|&pos| hooks[pos].priority);

    let mut groups: Vec<Vec<usize>> = Vec::new();
    for pos in order {
        match groups.last_mut() {
            Some(group) if hooks[group[0]].priority == hooks[pos].priority => group.push(pos),
            _ => groups.push(vec![pos]),
        }
    }
    groups
}

fn modified_matched(
    root: &Path,
    matched: &[std::path::PathBuf],
) -> anyhow::Result<Vec<std::path::PathBuf>> {
    let mut modified = Vec::new();
    for path in matched {
        if git::has_worktree_diff_in(root, path)? {
            modified.push(path.clone());
        }
    }
    Ok(modified)
}

/// Estimate the argv bytes consumed by everything except the matched files, so
/// `ARG_MAX` batching reserves the right headroom.
fn base_arg_len(hook: &Hook) -> usize {
    const FIXED: usize = 256; // program + shell wrapper + label
    let command_len = match &hook.command {
        HookCommand::Run(line) => line.len(),
        HookCommand::Script { path, runner } => path.len() + runner.as_ref().map_or(0, String::len),
    };
    let args_len: usize = hook.args.iter().map(|a| a.len() + 9).sum();
    FIXED + command_len + args_len
}
