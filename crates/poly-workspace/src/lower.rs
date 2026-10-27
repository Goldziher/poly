//! Lower a parsed `[hooks]` [`HooksConfig`] into the native `poly-hooks` model.
//!
//! For a requested git stage this produces a single [`StageSpec`] whose hook
//! list unifies, in priority order:
//!
//! 1. poly's **builtins** (`[hooks.builtin]`) whose resolved stages include the
//!    requested stage,
//! 2. the stage's **inline jobs** (`[[hooks.<stage>.jobs]]` / `.commands` /
//!    `.scripts`), and
//! 3. the **`[hooks.always]`** pseudo-stage jobs, appended to every concrete
//!    stage (lefthook "run everywhere").
//!
//! `[hooks.always]` is config-only — it never maps to a [`HookStage`]; its jobs
//! are expanded into each concrete stage during lowering.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use poly_config::{
    BuiltinHook, BuiltinJob, Guard, HookCacheMode, HooksConfig, Job, Patterns, Serial, Stage as ConfigStage,
    StageConfig, ToolsConfig,
};
use poly_hooks::Stage as HookStage;
use poly_hooks::filter::FilePattern;
use poly_hooks::identify::TagSet;
use poly_hooks::model::{CARGO_SERIAL_GROUP, Hook, HookCommand, SHARED_SERIAL_GROUP, StageSpec};
use poly_hooks::timeout::{ACCEPTED_TIMEOUT_FORMS, HookTimeout, parse_timeout};

use self::builtins::{PathProbe, ToolProbe};

pub(crate) use self::builtins::apply_cargo_fix_mode;

mod builtins;
mod cache;

/// Map a config [`ConfigStage`] to the runner's [`HookStage`].
///
/// Total **except** [`ConfigStage::Always`], which is an expand-only
/// pseudo-stage (its jobs are appended to every concrete stage during lowering)
/// and therefore has no runner counterpart — it returns `None`.
#[must_use]
pub fn to_hook_stage(stage: ConfigStage) -> Option<HookStage> {
    Some(match stage {
        ConfigStage::PreCommit => HookStage::PreCommit,
        ConfigStage::PreMergeCommit => HookStage::PreMergeCommit,
        ConfigStage::PrepareCommitMsg => HookStage::PrepareCommitMsg,
        ConfigStage::CommitMsg => HookStage::CommitMsg,
        ConfigStage::PostCommit => HookStage::PostCommit,
        ConfigStage::PreRebase => HookStage::PreRebase,
        ConfigStage::PostCheckout => HookStage::PostCheckout,
        ConfigStage::PostMerge => HookStage::PostMerge,
        ConfigStage::PrePush => HookStage::PrePush,
        ConfigStage::PostRewrite => HookStage::PostRewrite,
        ConfigStage::Manual => HookStage::Manual,
        ConfigStage::Always => return None,
    })
}

/// The config [`ConfigStage`] that a runner [`HookStage`] corresponds to.
///
/// Total over the runner's stages. The trailing wildcard exists only because
/// [`HookStage`] is `#[non_exhaustive]`; every variant in scope is mapped
/// explicitly and the stage-parity test guards against drift.
#[must_use]
pub fn from_hook_stage(stage: HookStage) -> ConfigStage {
    match stage {
        HookStage::PreCommit => ConfigStage::PreCommit,
        HookStage::PreMergeCommit => ConfigStage::PreMergeCommit,
        HookStage::PrepareCommitMsg => ConfigStage::PrepareCommitMsg,
        HookStage::CommitMsg => ConfigStage::CommitMsg,
        HookStage::PostCommit => ConfigStage::PostCommit,
        HookStage::PreRebase => ConfigStage::PreRebase,
        HookStage::PostCheckout => ConfigStage::PostCheckout,
        HookStage::PostMerge => ConfigStage::PostMerge,
        HookStage::PrePush => ConfigStage::PrePush,
        HookStage::PostRewrite => ConfigStage::PostRewrite,
        HookStage::Manual => ConfigStage::Manual,
        _ => ConfigStage::Manual,
    }
}

/// Build the [`StageSpec`] for `stage` from the parsed `[hooks]` config.
///
/// `poly_bin` is the absolute path of the running `poly` binary (used as the
/// entry for builtins); `files` is the resolved candidate file set, used only
/// for `{staged_files}` / `{all_files}` template substitution; `cache_mode` is
/// the global `[cache.results] hooks` mode, used to resolve each hook's
/// effective result-cache policy.
///
/// # Errors
///
/// Returns `Err` if a builtin's configured stage name is invalid, or a job's
/// file glob (or cache-input glob) fails to compile.
pub fn lower_stage(
    hooks: &HooksConfig,
    poly_bin: &Path,
    stage: HookStage,
    files: &[PathBuf],
    cache_mode: &HookCacheMode,
    root: &Path,
    tools: &ToolsConfig,
) -> Result<StageSpec> {
    lower_stage_with_git_args(hooks, poly_bin, stage, files, &[], cache_mode, root, tools)
}

/// [`lower_stage`] for a git-triggered run: `git_args` are the arguments git
/// passed to the hook, substituted into `{0}` / `{1}` / `{git_args}` in a job's
/// `run` line.
///
/// # Errors
///
/// Same as [`lower_stage`].
#[allow(clippy::too_many_arguments)]
pub fn lower_stage_with_git_args(
    hooks: &HooksConfig,
    poly_bin: &Path,
    stage: HookStage,
    files: &[PathBuf],
    git_args: &[String],
    cache_mode: &HookCacheMode,
    root: &Path,
    tools: &ToolsConfig,
) -> Result<StageSpec> {
    let inputs = TemplateInputs { files, git_args };
    lower_stage_with_inputs(hooks, poly_bin, stage, inputs, cache_mode, &PathProbe { root }, tools)
}

/// What a `run` line's `{...}` placeholders are substituted from.
#[derive(Clone, Copy)]
struct TemplateInputs<'a> {
    files: &'a [PathBuf],
    git_args: &'a [String],
}

/// [`lower_stage`] with an injectable capability [`ToolProbe`].
///
/// `lower_stage` calls this with the production [`PathProbe`]; tests pass a stub
/// so Cargo-builtin gating and `skip`/`only` guard evaluation are deterministic
/// regardless of the host toolchain.
#[cfg(test)]
fn lower_stage_with_probe(
    hooks: &HooksConfig,
    poly_bin: &Path,
    stage: HookStage,
    files: &[PathBuf],
    cache_mode: &HookCacheMode,
    probe: &dyn ToolProbe,
    tools: &ToolsConfig,
) -> Result<StageSpec> {
    let inputs = TemplateInputs { files, git_args: &[] };
    lower_stage_with_inputs(hooks, poly_bin, stage, inputs, cache_mode, probe, tools)
}

/// Lowering proper: [`lower_stage`] and [`lower_stage_with_git_args`] with an
/// injectable capability [`ToolProbe`].
fn lower_stage_with_inputs(
    hooks: &HooksConfig,
    poly_bin: &Path,
    stage: HookStage,
    inputs: TemplateInputs<'_>,
    cache_mode: &HookCacheMode,
    probe: &dyn ToolProbe,
    tools: &ToolsConfig,
) -> Result<StageSpec> {
    let config_stage = from_hook_stage(stage);
    let stage_config = hooks.stage_configs.get(&config_stage);

    if stage_config.is_some_and(|cfg| guard_skips(probe, cfg.skip.as_ref(), cfg.only.as_ref())) {
        return Ok(StageSpec {
            stage,
            ..StageSpec::default()
        });
    }

    let mut entries: Vec<Hook> = Vec::new();
    append_builtins(hooks, poly_bin, config_stage, cache_mode, probe, &mut entries)?;
    builtins::append_catalog_tools(tools, config_stage, probe, &mut entries)?;

    if let Some(cfg) = stage_config {
        append_jobs(hooks, stage, cfg, inputs, cache_mode, probe, &mut entries)?;
    }
    if let Some(always) = hooks.stage_configs.get(&ConfigStage::Always) {
        append_jobs(hooks, stage, always, inputs, cache_mode, probe, &mut entries)?;
    }

    for hook in &mut entries {
        hook.stage = stage;
    }

    entries.sort_by_key(|hook| hook.priority);

    let (precondition, before, after) = stage_steps(stage_config);
    Ok(StageSpec {
        stage,
        precondition,
        before,
        after,
        hooks: entries,
    })
}

/// Append poly's enabled builtins that resolve to `config_stage`.
///
/// Builtins use [`Hook::run`], whose `always_run` defaults to `false`: a builtin
/// is skipped when the matched file set is empty. This differs from an inline
/// job with no `files` filter (which sets `always_run = true`) — a deliberate
/// distinction, since `poly lint`/`fmt` over zero files is a no-op anyway.
///
/// The `lint` and `fmt` lines pass `--force-exclude` deliberately. A hook is
/// handed staged paths rather than deliberately named ones, so the repo's
/// `[discovery] exclude` must apply to them; the flag pins that on even in a repo
/// that set `[discovery] force_exclude = false`, which is a per-run convenience
/// for the direct CLI and not something a commit gate should inherit.
fn append_builtins(
    hooks: &HooksConfig,
    poly_bin: &Path,
    config_stage: ConfigStage,
    cache_mode: &HookCacheMode,
    probe: &dyn ToolProbe,
    out: &mut Vec<Hook>,
) -> Result<()> {
    let poly = shell_quote(&poly_bin.to_string_lossy());

    for (tool, builtin) in [(Tool::Lint, &hooks.builtin.lint), (Tool::Fmt, &hooks.builtin.fmt)] {
        if builtin.enabled {
            append_scoped_tool(tool, builtin, &poly, hooks, config_stage, cache_mode, out)?;
        }
    }
    if hooks.builtin.commit.enabled
        && builtin_runs_on(
            &hooks.builtin.commit.stages,
            &hooks.stages,
            ConfigStage::CommitMsg,
            config_stage,
        )?
    {
        out.push(Hook::run("poly-commit", format!("{poly} commit")));
    }
    builtins::append_file_safety(hooks, &poly, config_stage, out)?;
    builtins::append_cargo(hooks, config_stage, cache_mode, probe, out)?;
    Ok(())
}

/// Whether a builtin runs on `config_stage`, given its own `stages`, the global
/// default `stages`, and the builtin's own fallback when both are empty.
pub(super) fn builtin_runs_on(
    own_stages: &[String],
    default_stages: &[String],
    fallback: ConfigStage,
    config_stage: ConfigStage,
) -> Result<bool> {
    let raw = if !own_stages.is_empty() {
        own_stages
    } else if !default_stages.is_empty() {
        default_stages
    } else {
        return Ok(config_stage == fallback);
    };
    for name in raw {
        let parsed: ConfigStage = name
            .parse()
            .with_context(|| format!("invalid builtin hook stage `{name}`"))?;
        if parsed == config_stage {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The exclusion set every fix-mode builtin joins: two of them rewriting the same
/// file at once would race, so they run one at a time within a priority group.
const FIX_SERIAL_GROUP: &str = "fix";

#[derive(Clone, Copy)]
enum Tool {
    Lint,
    Fmt,
}

impl Tool {
    fn id(self) -> &'static str {
        match self {
            Tool::Lint => "lint",
            Tool::Fmt => "fmt",
        }
    }

    fn command(self, poly: &str, fix: bool) -> String {
        match (self, fix) {
            (Tool::Lint, false) => format!("{poly} lint --no-workspace --force-exclude"),
            (Tool::Lint, true) => format!("{poly} lint --no-workspace --force-exclude --fix"),
            (Tool::Fmt, false) => format!("{poly} fmt --check --force-exclude"),
            (Tool::Fmt, true) => format!("{poly} fmt --force-exclude --fix --exit-zero-on-fix"),
        }
    }
}

/// Lower one `lint` / `fmt` builtin: a single hook, or one per `jobs` entry.
///
/// A job's own `files` / `exclude` / `fix` / `priority` / `stages` replace the
/// builtin's. In fix mode the hook rewrites files and `stage_fixed` re-stages
/// them; otherwise it only checks.
fn append_scoped_tool(
    tool: Tool,
    builtin: &BuiltinHook,
    poly: &str,
    hooks: &HooksConfig,
    config_stage: ConfigStage,
    cache_mode: &HookCacheMode,
    out: &mut Vec<Hook>,
) -> Result<()> {
    let jobs = if builtin.jobs.is_empty() {
        vec![BuiltinJob::default()]
    } else {
        builtin.jobs.clone()
    };
    let scoped = !builtin.jobs.is_empty();
    for (index, job) in jobs.iter().enumerate() {
        let stages = if job.stages.is_empty() {
            &builtin.stages
        } else {
            &job.stages
        };
        if !builtin_runs_on(stages, &hooks.stages, ConfigStage::PreCommit, config_stage)? {
            continue;
        }
        let fix = job.fix.unwrap_or(builtin.fix);
        let id = match (&job.name, scoped) {
            (Some(name), _) => name.clone(),
            (None, true) => format!("{}-{}", tool.id(), index + 1),
            (None, false) => tool.id().to_string(),
        };
        let mut hook = Hook::run(id, tool.command(poly, fix));
        let (files, exclude) = builtin_globs(
            job.files.as_ref().or(builtin.files.as_ref()),
            job.exclude.as_ref().or(builtin.exclude.as_ref()),
        )?;
        hook.files = files;
        hook.exclude = exclude;
        hook.priority = job.priority.unwrap_or(builtin.priority);
        hook.stage_fixed = fix;
        hook.serial_group = fix.then(|| FIX_SERIAL_GROUP.to_string());
        hook.cache = cache::builtin_cache(cache_mode);
        out.push(hook);
    }
    Ok(())
}

fn append_jobs(
    hooks: &HooksConfig,
    stage: HookStage,
    cfg: &StageConfig,
    inputs: TemplateInputs<'_>,
    cache_mode: &HookCacheMode,
    probe: &dyn ToolProbe,
    out: &mut Vec<Hook>,
) -> Result<()> {
    for (label, job) in cfg.labeled_jobs() {
        if guard_skips(probe, job.skip.as_ref(), job.only.as_ref()) {
            continue;
        }
        if job_excluded_by_tags(job, &cfg.exclude_tags) {
            continue;
        }
        out.push(job_to_hook(hooks, stage, cfg, &label, job, inputs, cache_mode)?);
    }
    Ok(())
}

fn job_to_hook(
    hooks: &HooksConfig,
    stage: HookStage,
    cfg: &StageConfig,
    label: &str,
    job: &Job,
    inputs: TemplateInputs<'_>,
    cache_mode: &HookCacheMode,
) -> Result<Hook> {
    let mut env: BTreeMap<String, String> = hooks.env.clone();
    env.extend(job.env.iter().map(|(k, v)| (k.clone(), v.clone())));

    let include = collect_patterns(&[cfg.files.as_ref(), job.files.as_ref(), job.glob.as_ref()]);
    let exclude = collect_patterns(&[cfg.exclude.as_ref(), job.exclude.as_ref()]);
    let has_include = !include.is_empty();
    let files_pattern = build_glob(include)?;
    let exclude_pattern = build_glob(exclude)?;

    let types = (!job.file_types.is_empty()).then(|| TagSet::from_tags(&job.file_types));

    let scoped = filter_files(inputs.files, files_pattern.as_ref(), exclude_pattern.as_ref());
    let scoped = root_relative(job.root.as_deref(), scoped);
    let (command, template_pass_filenames) = build_command(job, &scoped, inputs.git_args)?;
    let pass_filenames = template_pass_filenames && !job.workspace;

    let cache = cache::job_cache(job, cache_mode)?;
    let compiler = job.cache.as_ref().is_some_and(|cache| cache.compiler);
    let timeout = job_timeout(stage, label, job)?;
    let (parallel, serial_group) = job_concurrency(cfg, job, &command);

    Ok(Hook {
        id: label.to_string(),
        stage,
        command,
        args: job.args.clone(),
        env,
        cwd: job.root.as_ref().map(std::path::PathBuf::from),
        show_output: job.show_output,
        precondition: job.precondition.clone(),
        before: job.before.as_ref().map(patterns_to_vec).unwrap_or_default(),
        files: files_pattern,
        exclude: exclude_pattern,
        types,
        priority: job.priority,
        cache,
        compiler,
        parallel,
        serial_group,
        require_serial: cfg.piped,
        fail_fast: cfg.piped,
        stage_fixed: job.stage_fixed,
        always_run: !has_include,
        pass_filenames,
        workspace: job.workspace,
        fail_text: job.fail_text.clone(),
        timeout,
        ..Hook::default()
    })
}

/// Resolve a job's concurrency: whether it may run beside a peer, and which
/// mutual-exclusion set it joins.
///
/// Precedence — the job's own `serial` key, then the stage, then the shape of
/// the job:
///
/// 1. `serial = "<name>"` / `true` / `false` — explicit, and outranks
///    everything below, including the automatic cargo grouping.
/// 2. `piped` — the stage's jobs are an ordered pipeline; `require_serial`
///    carries that (they share the shared set, in config order).
/// 3. A `run` line invoking **cargo** joins [`CARGO_SERIAL_GROUP`]. Cargo's own
///    package-cache and build-directory locks serialize these anyway; the set
///    only makes the queue visible and keeps each job's timeout budget starting
///    when the job does.
/// 4. `workspace = true` — a whole-project job runs **concurrently by default**.
///    Its peers are other whole-project tools, which is exactly the case that
///    has a whole run's wall-clock to gain from overlapping.
/// 5. Otherwise the stage's `parallel` flag decides, unchanged.
fn job_concurrency(cfg: &StageConfig, job: &Job, command: &HookCommand) -> (bool, Option<String>) {
    match &job.serial {
        Serial::Named(name) => (true, Some(name.clone())),
        Serial::Shared => (true, Some(SHARED_SERIAL_GROUP.to_string())),
        Serial::Off => (true, None),
        Serial::Unset if cfg.piped => (true, None),
        Serial::Unset if invokes_cargo(command) => (true, Some(CARGO_SERIAL_GROUP.to_string())),
        Serial::Unset if job.workspace => (true, None),
        Serial::Unset => (cfg.parallel, None),
    }
}

/// Whether a shell command line invokes `cargo` (or a `cargo-*` subcommand
/// binary) anywhere in it — after a `&&`, inside a pipeline, or by absolute
/// path.
///
/// Deliberately over-inclusive: the cost of a false positive is that one job
/// waits for another cargo job instead of blocking invisibly inside cargo's own
/// lock, and `serial = false` opts out. A script job is invisible here — poly
/// cannot read what a script runs — so a script that shells out to cargo should
/// name the set itself.
fn invokes_cargo(command: &HookCommand) -> bool {
    let HookCommand::Run(line) = command else {
        return false;
    };
    line.split(|c: char| c.is_whitespace() || matches!(c, ';' | '&' | '|' | '(' | ')' | '`'))
        .filter(|token| !token.is_empty())
        .any(|token| {
            let program = token.rsplit('/').next().unwrap_or(token);
            program == "cargo" || program.starts_with("cargo-")
        })
}

/// Resolve a job's `timeout` key into the runner's [`HookTimeout`].
///
/// The value is parsed by `poly-hooks`, so the config key and the environment
/// overrides accept exactly one grammar and can never drift apart. A value poly
/// cannot read is a **hard error** rather than a warning: a budget the author
/// believes is in force but which was silently discarded is the same false
/// promise as a key that is never consulted, and the error names the job
/// because "invalid timeout" in a repo with thirty hooks is not a diagnosis.
fn job_timeout(stage: HookStage, label: &str, job: &Job) -> Result<HookTimeout> {
    let Some(raw) = job.timeout.as_deref() else {
        return Ok(HookTimeout::Default);
    };
    parse_timeout(raw).ok_or_else(|| {
        anyhow::anyhow!(
            "hook job `{label}` in stage `{stage}`: invalid `timeout` value `{raw}`; expected {ACCEPTED_TIMEOUT_FORMS}"
        )
    })
}

/// The file set as a job running from `root` sees it: only files under `root`,
/// named relative to it. A job without a `root` keeps repo-relative names.
fn root_relative(root: Option<&str>, files: Vec<PathBuf>) -> Vec<PathBuf> {
    let Some(root) = root else {
        return files;
    };
    files
        .into_iter()
        .filter_map(|file| file.strip_prefix(root).map(std::path::Path::to_path_buf).ok())
        .collect()
}

/// Build the [`HookCommand`] and resolve whether the runner should append the
/// matched files. A `run` command containing `{staged_files}` / `{all_files}` /
/// `{git_args}` / `{N}` has those tokens substituted here and does **not**
/// receive appended files.
fn build_command(job: &Job, files: &[PathBuf], git_args: &[String]) -> Result<(HookCommand, bool)> {
    if let Some(run) = &job.run {
        if has_template(run) {
            return Ok((HookCommand::Run(substitute_templates(run, files, git_args)), false));
        }
        return Ok((HookCommand::Run(run.clone()), true));
    }
    if let Some(script) = &job.script {
        return Ok((
            HookCommand::Script {
                path: script.clone(),
                runner: job.runner.clone(),
            },
            true,
        ));
    }
    anyhow::bail!("hook job `{:?}` has neither `run` nor `script`", job.name)
}

/// Resolve `skip`/`only` guards to a skip decision, evaluating any
/// `{ run = … }` conditions through `probe`.
///
/// A guard is **active** when it is `Always(true)` or when any of its `run`
/// conditions exits 0 (lefthook's any-match rule). An active `skip` drops the
/// item; an inactive `only` drops it. Condition forms poly does not evaluate
/// (`ref`, bare git-operation names) never reach here — `HooksConfig::validate`
/// rejects them at load time rather than let a guard be a silent no-op.
fn guard_skips(probe: &dyn ToolProbe, skip: Option<&Guard>, only: Option<&Guard>) -> bool {
    if skip.is_some_and(|guard| guard_active(probe, guard)) {
        return true;
    }
    if only.is_some_and(|guard| !guard_active(probe, guard)) {
        return true;
    }
    false
}

/// Whether a guard is currently active.
fn guard_active(probe: &dyn ToolProbe, guard: &Guard) -> bool {
    match guard {
        Guard::Always(value) => *value,
        Guard::Conditions(_) => guard
            .shell_conditions()
            .into_iter()
            .any(|command| probe.guard_passes(command)),
    }
}

fn job_excluded_by_tags(job: &Job, exclude_tags: &[String]) -> bool {
    job.tags.iter().any(|tag| exclude_tags.contains(tag))
}

fn collect_patterns(sources: &[Option<&Patterns>]) -> Vec<String> {
    let mut out = Vec::new();
    for source in sources.iter().flatten() {
        out.extend(source.as_slice().iter().cloned());
    }
    out
}

fn build_glob(patterns: Vec<String>) -> Result<Option<FilePattern>> {
    if patterns.is_empty() {
        return Ok(None);
    }
    Ok(Some(
        FilePattern::glob(patterns).context("invalid hook file glob pattern")?,
    ))
}

/// Compile a builtin hook's `files`/`exclude` patterns into the runner's
/// include/exclude [`FilePattern`]s. A `None` include matches every candidate
/// file; the runner applies both filters to the matched set before the builtin
/// receives it — mirroring the per-hook `exclude` of the prek config.
pub(super) fn builtin_globs(
    files: Option<&Patterns>,
    exclude: Option<&Patterns>,
) -> Result<(Option<FilePattern>, Option<FilePattern>)> {
    let include = build_glob(collect_patterns(&[files]))?;
    let exclude = build_glob(collect_patterns(&[exclude]))?;
    Ok((include, exclude))
}

/// Scope `files` to a job's compiled include/exclude globs.
///
/// A `None` include matches everything (an unfiltered job); `exclude` removes
/// matches. Used only for template substitution — when `pass_filenames` is left
/// enabled the runner applies the same filter itself.
fn filter_files(files: &[PathBuf], include: Option<&FilePattern>, exclude: Option<&FilePattern>) -> Vec<PathBuf> {
    files
        .iter()
        .filter(|path| include.is_none_or(|pattern| pattern.is_match(path.as_path())))
        .filter(|path| exclude.is_none_or(|pattern| !pattern.is_match(path.as_path())))
        .cloned()
        .collect()
}

fn has_template(run: &str) -> bool {
    placeholders(run).next().is_some()
}

/// The `{name}` placeholders poly substitutes in `run`, as `(start, end, name)`.
///
/// The names are the file lists (`staged_files`, `all_files`), the joined git
/// arguments (`0`, `git_args`) and a single git argument (`1`, `2`, ...). A
/// `${VAR}` / `${1}` shell expansion is not a placeholder.
fn placeholders(run: &str) -> impl Iterator<Item = (usize, usize, &str)> {
    run.match_indices('{').filter_map(move |(start, _)| {
        if run[..start].ends_with('$') {
            return None;
        }
        let len = run[start + 1..].find('}')?;
        let name = &run[start + 1..start + 1 + len];
        let numeric = !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit());
        (numeric || matches!(name, "staged_files" | "all_files" | "git_args")).then_some((start, start + len + 2, name))
    })
}

/// Substitute the placeholders in `run`.
///
/// `{staged_files}` / `{all_files}` resolve to the same shell-quoted, space-joined
/// candidate set (the caller decides whether that is the staged or whole-tree set
/// via `--all-files`). `{0}` and `{git_args}` are every git hook argument,
/// quoted and space-joined; `{N}` is the N-th (1-based) or empty when git passed
/// fewer.
fn substitute_templates(run: &str, files: &[PathBuf], git_args: &[String]) -> String {
    let quote_all = |items: Vec<String>| items.iter().map(|item| shell_quote(item)).collect::<Vec<_>>().join(" ");
    let joined_files = quote_all(files.iter().map(|path| path.to_string_lossy().into_owned()).collect());
    let joined_args = quote_all(git_args.to_vec());
    let mut out = String::with_capacity(run.len());
    let mut cursor = 0;
    for (start, end, name) in placeholders(run) {
        let replacement = match name {
            "staged_files" | "all_files" => joined_files.clone(),
            "0" | "git_args" => joined_args.clone(),
            index => index
                .parse::<usize>()
                .ok()
                .and_then(|n| n.checked_sub(1))
                .and_then(|n| git_args.get(n))
                .map(|arg| shell_quote(arg))
                .unwrap_or_default(),
        };
        out.push_str(&run[cursor..start]);
        out.push_str(&replacement);
        cursor = end;
    }
    out.push_str(&run[cursor..]);
    out
}

fn stage_steps(cfg: Option<&StageConfig>) -> (Option<String>, Vec<String>, Vec<String>) {
    match cfg {
        None => (None, Vec::new(), Vec::new()),
        Some(cfg) => (
            cfg.precondition.clone(),
            cfg.before.as_ref().map(patterns_to_vec).unwrap_or_default(),
            cfg.after.as_ref().map(patterns_to_vec).unwrap_or_default(),
        ),
    }
}

fn patterns_to_vec(patterns: &Patterns) -> Vec<String> {
    patterns.as_slice().to_vec()
}

/// Single-quote a string for safe interpolation into the `sh -c` command line
/// the runner builds. On Windows the runner uses `cmd /C`, which has no
/// single-quote semantics, so a double-quote wrap is used there.
#[cfg(not(windows))]
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(windows)]
fn shell_quote(value: &str) -> String {
    let escaped = value.replace('%', "%%").replace('"', "\"\"");
    format!("\"{escaped}\"")
}

#[cfg(test)]
mod tests;
