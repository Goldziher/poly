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

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Once;
use std::time::{Duration, Instant};

use indicatif::ProgressBar;
use poly_cache::{CacheKey, InputDigest, Namespace, ResultCache};
use rayon::prelude::*;
use tracing::warn;

use crate::filter::{FilePattern, FileTagCache, HookFileFilter};
use crate::git;
use crate::model::{
    Hook, HookCache, HookCommand, HookOutcome, HookRunOutcome, HookRunRequest, HookStatus, SccacheSettings, SkipReason,
    StageOutcome, StageSpec, StageStatus, StepOutcome,
};
use crate::process::Cmd;
use crate::reporter::{CaptureSink, HookBar, PreviewSink, ProgressUi};
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
    let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build()?;
    // A live multi-line progress display, only when progress was requested (the
    // CLI enables it for an interactive stderr). The draw target self-hides on a
    // non-terminal, so downstream code never special-cases it.
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

/// A hook's resolved file set and skip decision, computed before execution.
struct Prepared {
    matched: Vec<std::path::PathBuf>,
    skip: Option<SkipReason>,
}

fn run_stage(request: &HookRunRequest, spec: &StageSpec, ui: Option<&ProgressUi>) -> anyhow::Result<StageOutcome> {
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
    let (mut hooks, any_failed) = run_hooks(request, spec, &prepared, ui)?;
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
    ui: Option<&ProgressUi>,
) -> anyhow::Result<(Vec<HookOutcome>, bool)> {
    let mut collected = Vec::with_capacity(spec.hooks.len());
    let mut any_failed = false;

    for group in group_by_priority(&spec.hooks) {
        let serial = group.iter().any(|&pos| spec.hooks[pos].is_serial());
        let outcomes = run_group(request, spec, prepared, &group, serial, ui);

        let mut abort = false;
        for (&pos, (mut outcome, store_key)) in group.iter().zip(outcomes) {
            let hook = &spec.hooks[pos];
            let passed = matches!(outcome.status, HookStatus::Passed);

            // The matched-file modification set is needed for `stage_fixed`
            // re-staging *and* to gate caching: a hook that mutated its inputs
            // must never be stored. Compute it once, only when it can matter.
            let mut modified = if passed && (hook.stage_fixed || store_key.is_some()) {
                modified_matched(&request.root, &prepared[pos].matched)?
            } else {
                Vec::new()
            };

            if passed && hook.stage_fixed && !modified.is_empty() {
                git::add(&request.root, &modified)?;
                outcome.files_modified = true;
            }

            // For `DeclaredInputs` caching the digested set differs from
            // `matched`; a mutation to a declared input outside `matched` must
            // also block storing, or a later hit would drop that side effect.
            if modified.is_empty() && store_key.is_some() {
                if let HookCache::DeclaredInputs(pattern) = &hook.cache {
                    let declared = declared_input_files(&request.root, pattern)?;
                    modified = modified_matched(&request.root, &declared)?;
                }
            }

            // Store only a passing, tree-clean run. `store_key` is `Some` only on
            // a cache miss for a cacheable hook, so the bytes are written once. A
            // store failure (full / read-only cache) must not fail the hook run.
            if let (Some(cache), Some(key)) = (request.cache.as_ref(), &store_key) {
                if passed && modified.is_empty() {
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

/// Run one priority group, returning each hook's outcome paired with the cache
/// key under which it should be stored (`Some` only on a cache miss for a
/// cacheable hook; `None` for skips, hits, and non-cacheable hooks).
fn run_group(
    request: &HookRunRequest,
    spec: &StageSpec,
    prepared: &[Prepared],
    group: &[usize],
    serial: bool,
    ui: Option<&ProgressUi>,
) -> Vec<(HookOutcome, Option<CacheKey>)> {
    let run_one = |&pos: &usize| -> (HookOutcome, Option<CacheKey>) {
        let hook = &spec.hooks[pos];
        if let Some(reason) = &prepared[pos].skip {
            return (skipped_outcome(hook, pos, reason.clone()), None);
        }
        let matched = &prepared[pos].matched;

        // Derive the key once (reading input bytes at most once); `None` when
        // caching is off, the hook is not cacheable, or an input is unreadable.
        // A workspace hook under isolation is keyed on STAGED bytes (the
        // snapshot), not the worktree — otherwise reverting an unstaged edit
        // could replay a stale pass computed against different staged content.
        let content_root = match (hook.workspace, request.work_root.as_deref()) {
            (true, Some(snapshot)) => snapshot,
            _ => request.root.as_path(),
        };
        let key = request
            .cache
            .as_ref()
            .and_then(|_| cache_key(&request.root, content_root, hook, matched));

        // Lookup: a hit short-circuits execution. Only passing, tree-clean runs
        // are ever stored, so a hit always means "passed cleanly".
        if let (Some(cache), Some(key)) = (request.cache.as_ref(), key.as_ref()) {
            if let Some(output) = cache.get(Namespace::Hook, key) {
                return (cached_outcome(hook, pos, output), None);
            }
        }

        let refs: Vec<&Path> = matched.iter().map(AsRef::as_ref).collect();
        // A cache hit or a skip returns above without ever reaching here, so a
        // spinner is started only for hooks whose body actually executes.
        let hook_bar = ui.map(|ui| ui.start(&hook.id));
        // A whole-workspace hook runs from the staged snapshot when the run
        // carries one, isolating it to staged content; per-file hooks always run
        // from the real root. When isolated, point cargo at the real `target/`
        // so third-party dependency artifacts are reused instead of rebuilt.
        let (exec_root, cargo_target_dir) = match (hook.workspace, request.work_root.as_deref()) {
            (true, Some(snapshot)) => (snapshot, Some(request.root.join("target"))),
            _ => (request.root.as_path(), None),
        };
        let outcome = run_hook(
            exec_root,
            hook,
            pos,
            &refs,
            request.sccache.as_ref(),
            cargo_target_dir.as_deref(),
            hook_bar.as_ref().map(HookBar::bar),
        );
        if let (Some(ui), Some(bar)) = (ui, hook_bar.as_ref()) {
            ui.finish(bar, outcome.status.is_failure(), outcome.duration);
        }
        (outcome, key)
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
fn run_hook(
    root: &Path,
    hook: &Hook,
    position: usize,
    matched: &[&Path],
    sccache: Option<&SccacheSettings>,
    cargo_target_dir: Option<&Path>,
    bar: Option<&ProgressBar>,
) -> HookOutcome {
    let start = Instant::now();
    let base_len = base_arg_len(hook);
    let batches = crate::concurrency::partition_files(matched, base_len);

    let results: Vec<(HookStatus, Vec<u8>)> = batches
        .into_par_iter()
        .map(|batch| {
            execute(
                build_command(hook, root, batch, sccache, cargo_target_dir),
                bar,
                &hook.id,
            )
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
        files_modified: false,
        output,
        duration: start.elapsed(),
        cached: false,
    }
}

fn skipped_outcome(hook: &Hook, position: usize, reason: SkipReason) -> HookOutcome {
    HookOutcome {
        id: hook.id.clone(),
        position,
        status: HookStatus::Skipped(reason),
        files_modified: false,
        output: Vec::new(),
        duration: Duration::ZERO,
        cached: false,
    }
}

/// Build the outcome for a hook served from the result cache: a passing,
/// zero-duration run carrying the stored output bytes.
fn cached_outcome(hook: &Hook, position: usize, output: Vec<u8>) -> HookOutcome {
    HookOutcome {
        id: hook.id.clone(),
        position,
        status: HookStatus::Passed,
        files_modified: false,
        output,
        duration: Duration::ZERO,
        cached: true,
    }
}

// ── Command construction & execution ────────────────────────────────────────

fn build_command(
    hook: &Hook,
    root: &Path,
    files: &[&Path],
    sccache: Option<&SccacheSettings>,
    cargo_target_dir: Option<&Path>,
) -> Cmd {
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
    // A per-hook `cwd` overrides the repo root (resolved relative to root so
    // relative paths like `"packages/go"` work as expected).
    let effective_cwd = hook
        .cwd
        .as_deref()
        .map_or_else(|| root.to_path_buf(), |rel| root.join(rel));
    cmd.current_dir(&effective_cwd);
    cmd.envs(hook.env.iter());
    // Under staged isolation, redirect cargo's build cache to the real repo's
    // `target/` so unchanged dependencies are not recompiled from the snapshot.
    // A hook-level `CARGO_TARGET_DIR` in `hook.env` (rare) wins.
    if let Some(target) = cargo_target_dir {
        if !hook.env.contains_key("CARGO_TARGET_DIR") {
            cmd.env("CARGO_TARGET_DIR", target);
        }
    }
    inject_sccache_env(&mut cmd, hook, sccache);
    cmd
}

/// Module-global guard so the shared sccache server is started at most once per
/// `poly hooks` process, no matter how many compiler hooks or batches run.
static SCCACHE_SERVER_START: Once = Once::new();

/// Inject the tier-2 sccache environment into a compiler hook's command.
///
/// A no-op unless the hook opted in via [`Hook::compiler`] **and** the run
/// carries [`SccacheSettings`]. Starts the shared sccache server once per
/// process (best-effort — a start failure only warns, since sccache also
/// auto-starts on first client use), then sets `RUSTC_WRAPPER` plus the
/// optional `SCCACHE_DIR` / `SCCACHE_CACHE_SIZE`.
///
/// Caveat: if an sccache server is already running with a different `SCCACHE_DIR`
/// / size, the client env is ignored by that server — this is accepted.
fn inject_sccache_env(cmd: &mut Cmd, hook: &Hook, sccache: Option<&SccacheSettings>) {
    if !hook.compiler {
        return;
    }
    let Some(settings) = sccache else {
        return;
    };
    ensure_sccache_server(settings);
    cmd.env("RUSTC_WRAPPER", &settings.bin);
    if let Some(dir) = &settings.dir {
        cmd.env("SCCACHE_DIR", dir);
    }
    if let Some(max_size) = &settings.max_size {
        cmd.env("SCCACHE_CACHE_SIZE", max_size);
    }
}

/// Start the sccache server idempotently (once per process), with the resolved
/// `SCCACHE_DIR` / `SCCACHE_CACHE_SIZE` in its own environment. Best-effort: a
/// launch failure is logged and ignored.
fn ensure_sccache_server(settings: &SccacheSettings) {
    SCCACHE_SERVER_START.call_once(|| {
        let mut cmd = Cmd::new(&settings.bin, format!("{} --start-server", settings.bin));
        cmd.arg("--start-server");
        if let Some(dir) = &settings.dir {
            cmd.env("SCCACHE_DIR", dir);
        }
        if let Some(max_size) = &settings.max_size {
            cmd.env("SCCACHE_CACHE_SIZE", max_size);
        }
        cmd.check(false).stdout(Stdio::null()).stderr(Stdio::null());
        if let Err(error) = cmd.status() {
            warn!("failed to start sccache server: {error}");
        }
    });
}

#[cfg(not(windows))]
fn shell_command(line: &str, args: &[String], files: &[&Path], pass_filenames: bool) -> Cmd {
    // `sh -c '<line> "$@"' poly-hook <args> <files>` — args and matched files
    // become the positional parameters consumed by `"$@"`. `$0` is a label.
    let mut cmd = Cmd::new(SHELL, line.to_string());
    cmd.arg(SHELL_ARG).arg(format!("{line} \"$@\"")).arg("poly-hook");
    cmd.args(args);
    if pass_filenames {
        cmd.args(files.iter().map(|p| p.as_os_str()));
    }
    cmd
}

/// Quote a token for inclusion in a `cmd /C` command line so an
/// attacker-controlled value (notably a tracked filename like `foo & evil.exe`)
/// cannot inject cmd.exe syntax. Wrap in double quotes — which neutralizes the
/// metacharacters cmd interprets outside quotes (`&`, `|`, `<`, `>`, `(`, `)`,
/// whitespace) — doubling any embedded `"` and escaping `%`.
///
/// Kept un-gated so the quoting logic is unit-tested on every platform; it is
/// only *called* from the `cfg(windows)` `shell_command` below.
#[cfg_attr(not(windows), allow(dead_code))]
fn cmd_quote(value: &str) -> String {
    let escaped = value.replace('"', "\"\"").replace('%', "%%");
    format!("\"{escaped}\"")
}

#[cfg(windows)]
fn shell_command(line: &str, args: &[String], files: &[&Path], pass_filenames: bool) -> Cmd {
    // `cmd /C` has no `"$@"`, so join the command, args, and files into one line.
    // `line` is the author's command (trusted); args and matched files are
    // quoted so they are passed as literal tokens — matching the Unix branch,
    // where they reach the program as separate argv and are never reinterpreted.
    let mut joined = line.to_string();
    for arg in args {
        joined.push(' ');
        joined.push_str(&cmd_quote(arg));
    }
    if pass_filenames {
        for file in files {
            joined.push(' ');
            joined.push_str(&cmd_quote(&file.to_string_lossy()));
        }
    }
    let mut cmd = Cmd::new(SHELL, line.to_string());
    cmd.arg(SHELL_ARG).arg(joined);
    cmd
}

/// Run one command to completion, capturing its combined output. When `bar` is
/// present the output is streamed live into the hook's spinner via a
/// [`PreviewSink`]; otherwise a plain [`CaptureSink`] just accumulates it.
fn execute(mut cmd: Cmd, bar: Option<&ProgressBar>, id: &str) -> (HookStatus, Vec<u8>) {
    cmd.check(false);
    let (result, bytes) = if let Some(bar) = bar {
        let mut sink = PreviewSink::new(bar, id);
        let result = cmd.output_with_sink(&mut sink);
        (result, sink.into_bytes())
    } else {
        let mut sink = CaptureSink::default();
        let result = cmd.output_with_sink(&mut sink);
        (result, sink.into_bytes())
    };
    match result {
        Ok(output) => {
            let status = if output.status.success() {
                HookStatus::Passed
            } else {
                HookStatus::Failed {
                    code: output.status.code(),
                }
            };
            (status, bytes)
        }
        Err(error) => (HookStatus::Error(error.to_string()), bytes),
    }
}

fn run_step(root: &Path, command: &str) -> StepOutcome {
    let mut cmd = Cmd::new(SHELL, command.to_string());
    cmd.arg(SHELL_ARG).arg(command).current_dir(root);
    let (status, output) = execute(cmd, None, command);
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

fn prepare_one(request: &HookRunRequest, hook: &Hook, all_paths: &[&Path], tag_cache: &FileTagCache<'_>) -> Prepared {
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
            let has_tag_filter = hook.types.is_some() || hook.types_or.is_some() || hook.exclude_types.is_some();
            let matched: Vec<std::path::PathBuf> = all_paths
                .iter()
                .filter(|path| {
                    filter.matches_filename(path) && (!has_tag_filter || filter.matches_tags(tag_cache.tags_for(path)))
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

fn modified_matched(root: &Path, matched: &[std::path::PathBuf]) -> anyhow::Result<Vec<std::path::PathBuf>> {
    let mut modified = Vec::new();
    for path in matched {
        if git::has_worktree_diff_in(root, path)? {
            modified.push(path.clone());
        }
    }
    Ok(modified)
}

// ── Result-cache key derivation ─────────────────────────────────────────────

/// Derive the [`Namespace::Hook`] cache key for `hook`, or `None` when the hook
/// is not cacheable or its inputs cannot be read.
///
/// The key folds in the hook id, a command-identity `version`, the declared
/// environment (as the `args` table), and a content digest of the relevant
/// input files — so a changed command, env, or input invalidates the entry.
///
/// `git_root` (the real repository) resolves the input *file set* — via
/// `git ls-files` / the matched paths — while `content_root` supplies the
/// *bytes* that are digested. They differ for a workspace hook under isolation,
/// where the list is the tracked tree but the content is the staged snapshot.
fn cache_key(git_root: &Path, content_root: &Path, hook: &Hook, matched: &[PathBuf]) -> Option<CacheKey> {
    let digest = match &hook.cache {
        HookCache::Disabled => return None,
        HookCache::MatchedFiles => matched_files_digest(content_root, matched)?,
        HookCache::DeclaredInputs(pattern) => declared_inputs_digest(git_root, content_root, pattern)?,
    };
    let version = hook_version(hook);
    let args = hook_env_table(hook);
    Some(ResultCache::key(Namespace::Hook, &hook.id, &version, &args, &digest))
}

/// Digest the hook's matched files (each as `(relative_path, bytes)`), reading
/// bytes from `content_root`.
///
/// Returns `None` if any matched file cannot be read, which skips caching this
/// hook rather than risk a key derived from partial inputs.
fn matched_files_digest(content_root: &Path, matched: &[PathBuf]) -> Option<InputDigest> {
    read_digest(content_root, matched.iter().cloned())
}

/// Digest every tracked file matching `pattern` — the file set resolved against
/// the whole tree (`git ls-files` under `git_root`), the bytes read from
/// `content_root`.
///
/// Returns `None` if the tree cannot be listed or a matching file is unreadable.
fn declared_inputs_digest(git_root: &Path, content_root: &Path, pattern: &FilePattern) -> Option<InputDigest> {
    let selected = declared_input_files(git_root, pattern).ok()?;
    read_digest(content_root, selected.into_iter())
}

/// The tracked files matching a `DeclaredInputs` pattern (`git ls-files` filtered
/// by the glob). Used both for the digest and the cache-store mutation guard.
fn declared_input_files(root: &Path, pattern: &FilePattern) -> anyhow::Result<Vec<PathBuf>> {
    Ok(git::list_files(root)?
        .into_iter()
        .filter(|path| pattern.is_match(path))
        .collect())
}

/// Read the given repo-relative paths and fold them into an [`InputDigest`],
/// sorted by path for a deterministic key. `None` if any read fails.
fn read_digest(content_root: &Path, paths: impl Iterator<Item = PathBuf>) -> Option<InputDigest> {
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    for path in paths {
        let bytes = std::fs::read(content_root.join(&path)).ok()?;
        files.push((path.to_string_lossy().into_owned(), bytes));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Some(ResultCache::file_set_digest(
        files.iter().map(|(path, bytes)| (path.as_str(), bytes.as_slice())),
    ))
}

/// A string capturing the hook's command identity, so a changed command, script
/// target, argument list, or file-passing mode invalidates the cache key.
fn hook_version(hook: &Hook) -> String {
    use std::fmt::Write as _;
    // Build the identity string in one buffer — no `line.clone()` or
    // `args.join("\0")` intermediates. The produced bytes are identical to the
    // previous `format!`, so existing cache keys stay valid.
    let mut version = String::new();
    match &hook.command {
        HookCommand::Run(line) => version.push_str(line),
        // Writing into a String is infallible.
        HookCommand::Script { path, runner } => {
            let _ = write!(version, "script\0{runner:?}\0{path}");
        }
    }
    version.push('\0');
    for (index, arg) in hook.args.iter().enumerate() {
        if index > 0 {
            version.push('\0');
        }
        version.push_str(arg);
    }
    // `pass_filenames` changes the effective argv (per-file vs aggregate), so a
    // toggle must invalidate even when command, args, env, and inputs are equal.
    let _ = write!(version, "\0pass_filenames={}", hook.pass_filenames);
    version
}

/// The hook's declared environment as a TOML table, so an env change invalidates
/// the cache key. The `BTreeMap` is already ordered, giving a stable table.
fn hook_env_table(hook: &Hook) -> toml::Table {
    hook.env
        .iter()
        .map(|(key, value)| (key.clone(), toml::Value::String(value.clone())))
        .collect()
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;

    use super::{Hook, SccacheSettings, build_command, cmd_quote};

    #[test]
    fn cmd_quote_neutralizes_metacharacters() {
        // A filename with a cmd.exe command separator must come back wrapped in
        // quotes so it is a single literal token, not `evil.exe` as a command.
        assert_eq!(cmd_quote("foo.rs & evil.exe"), "\"foo.rs & evil.exe\"");
        // Embedded quotes are doubled; percent is escaped.
        assert_eq!(cmd_quote("a\"b"), "\"a\"\"b\"");
        assert_eq!(cmd_quote("100%done"), "\"100%%done\"");
    }

    /// Collect the explicit environment overrides a built [`super::Cmd`] carries.
    fn injected_env(hook: &Hook, sccache: Option<&SccacheSettings>) -> HashMap<String, String> {
        let cmd = build_command(hook, Path::new("."), &[], sccache, None);
        cmd.get_envs()
            .filter_map(|(key, value)| {
                value.map(|value| (key.to_string_lossy().into_owned(), value.to_string_lossy().into_owned()))
            })
            .collect()
    }

    /// `bin = "true"` keeps the one-shot `--start-server` probe harmless: `true`
    /// ignores its arguments and exits 0, so the test never requires sccache.
    fn settings() -> SccacheSettings {
        SccacheSettings {
            bin: "true".to_string(),
            dir: Some(std::path::PathBuf::from("/tmp/sccache-test")),
            max_size: Some("2G".to_string()),
        }
    }

    #[test]
    fn compiler_hook_gets_sccache_env_injected() {
        let mut hook = Hook::run("clippy", "cargo clippy");
        hook.compiler = true;
        let env = injected_env(&hook, Some(&settings()));
        assert_eq!(env.get("RUSTC_WRAPPER").map(String::as_str), Some("true"));
        assert_eq!(env.get("SCCACHE_DIR").map(String::as_str), Some("/tmp/sccache-test"));
        assert_eq!(env.get("SCCACHE_CACHE_SIZE").map(String::as_str), Some("2G"));
    }

    #[test]
    fn non_compiler_hook_gets_no_sccache_env() {
        let hook = Hook::run("fmt", "cargo fmt --check");
        let env = injected_env(&hook, Some(&settings()));
        assert!(!env.contains_key("RUSTC_WRAPPER"), "env: {env:?}");
        assert!(!env.contains_key("SCCACHE_DIR"), "env: {env:?}");
    }

    #[test]
    fn compiler_hook_without_settings_gets_no_sccache_env() {
        let mut hook = Hook::run("clippy", "cargo clippy");
        hook.compiler = true;
        let env = injected_env(&hook, None);
        assert!(!env.contains_key("RUSTC_WRAPPER"), "env: {env:?}");
    }

    #[test]
    fn sccache_settings_without_dir_omits_dir_env() {
        let mut hook = Hook::run("clippy", "cargo clippy");
        hook.compiler = true;
        let bare = SccacheSettings {
            bin: "true".to_string(),
            dir: None,
            max_size: None,
        };
        let env = injected_env(&hook, Some(&bare));
        assert_eq!(env.get("RUSTC_WRAPPER").map(String::as_str), Some("true"));
        assert!(!env.contains_key("SCCACHE_DIR"), "env: {env:?}");
        assert!(!env.contains_key("SCCACHE_CACHE_SIZE"), "env: {env:?}");
    }
}
