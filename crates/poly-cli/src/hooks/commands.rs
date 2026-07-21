//! `poly hooks` — clap subcommands over the native `poly-hooks` runner.
//!
//! poly no longer shells out to an external hook engine: `[hooks]` is lowered
//! (see [`crate::hooks::lower`]) into the in-process [`poly_hooks`] model and
//! executed by [`poly_hooks::run`]. The subcommands are:
//!
//! - `poly hooks run [STAGE]` — run one stage's hooks (default: the configured
//!   `stages`, else `pre-commit`).
//! - `poly hooks install` / `uninstall` — manage the `.git/hooks` shims.
//! - `poly hooks hook-impl --hook-type=<type> -- <git args>` — the entry point
//!   the installed shim invokes when a git hook fires.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Args, Subcommand, ValueEnum as _};
use owo_colors::{OwoColorize, Stream::Stdout};
use poly_cache::ResultCache;
use poly_config::{PolyConfig, Stage as ConfigStage};
use poly_hooks::snapshot::StagedSnapshot;
use poly_hooks::stage::RunInputMode;

use crate::hooks::checks::{self, CheckArgs};
use crate::hooks::lower;

/// `poly hooks` arguments — an optional subcommand (defaulting to `run`).
#[derive(Args)]
pub struct HooksArgs {
    /// The hooks operation to perform (default: `run`).
    #[command(subcommand)]
    pub command: Option<HooksCommand>,
}

/// The `poly hooks` subcommands.
#[derive(Subcommand)]
pub enum HooksCommand {
    /// Run a stage's hooks (default: the configured `stages`, else `pre-commit`).
    Run(RunArgs),
    /// Install poly's git-hook shims into `.git/hooks`.
    Install(InstallArgs),
    /// Remove poly's git-hook shims, restoring any preserved hook.
    Uninstall(UninstallArgs),
    /// Refresh remote hook sources and update poly-hooks.lock.
    Update,
    /// Internal: invoked by an installed shim when a git hook fires.
    #[command(name = "hook-impl")]
    HookImpl(HookImplArgs),
    /// Internal: run the pure-Rust file-safety checks over the given files.
    ///
    /// The `file_safety` builtin lowers to this subcommand; it is not part of
    /// the user-facing surface, so it is hidden from `--help`.
    #[command(hide = true)]
    Check(CheckArgs),
}

/// `poly hooks run` arguments.
#[derive(Args, Default)]
pub struct RunArgs {
    /// Stage to run (accepts aliases: `commit`, `push`, `merge-commit`).
    pub stage: Option<String>,

    /// Path to the config file (default: nearest poly.toml).
    #[arg(long)]
    pub config: Option<PathBuf>,

    /// Run over the whole tracked tree rather than just the staged files.
    #[arg(long)]
    pub all_files: bool,

    /// Commit-message file for message-file stages (`commit-msg`,
    /// `prepare-commit-msg`); required when running those stages directly.
    #[arg(long)]
    pub message_file: Option<PathBuf>,

    /// Number of parallel jobs (default: env / all logical cores).
    #[arg(short = 'j', long)]
    pub jobs: Option<usize>,

    /// Bypass the result cache for this run (neither read nor write).
    #[arg(long)]
    pub no_cache: bool,

    /// Disable tier-2 sccache env injection for compiler hooks this run.
    #[arg(long)]
    pub no_sccache: bool,
}

/// `poly hooks install` arguments.
#[derive(Args)]
pub struct InstallArgs {
    /// Hook types to install (default: the stages your `poly.toml` configures).
    #[arg(long = "hook-type", value_enum)]
    pub hook_types: Vec<poly_hooks::HookType>,

    /// Overwrite an existing hook, discarding any preserved legacy hook.
    #[arg(long)]
    pub overwrite: bool,
}

/// `poly hooks uninstall` arguments.
#[derive(Args)]
pub struct UninstallArgs {
    /// Hook types to uninstall (default: every git-triggered hook type).
    #[arg(long = "hook-type", value_enum)]
    pub hook_types: Vec<poly_hooks::HookType>,
}

/// `poly hooks hook-impl` arguments — supplied by the installed shim.
#[derive(Args)]
pub struct HookImplArgs {
    /// The git hook type that fired.
    #[arg(long = "hook-type", value_enum)]
    pub hook_type: poly_hooks::HookType,

    /// Path to the config file (default: nearest poly.toml).
    #[arg(long)]
    pub config: Option<PathBuf>,

    /// Number of parallel jobs (default: env / all logical cores).
    #[arg(short = 'j', long)]
    pub jobs: Option<usize>,

    /// Bypass the result cache for this run (neither read nor write).
    #[arg(long)]
    pub no_cache: bool,

    /// Disable tier-2 sccache env injection for compiler hooks this run.
    #[arg(long)]
    pub no_sccache: bool,

    /// The raw git hook arguments, passed after `--`.
    #[arg(last = true)]
    pub git_args: Vec<OsString>,
}

/// Run `poly hooks`, mapping any error to exit code 2.
pub fn run_hooks(args: HooksArgs) -> ExitCode {
    let result = match args.command {
        None => run_stage(RunArgs::default()),
        Some(HooksCommand::Run(run_args)) => run_stage(run_args),
        Some(HooksCommand::Install(install_args)) => install(install_args),
        Some(HooksCommand::Uninstall(uninstall_args)) => uninstall(uninstall_args),
        Some(HooksCommand::Update) => update_sources(),
        Some(HooksCommand::HookImpl(hook_impl_args)) => hook_impl(hook_impl_args),
        Some(HooksCommand::Check(check_args)) => checks::run_file_safety_checks(&check_args),
    };
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("poly hooks: {error:#}");
            ExitCode::from(2)
        }
    }
}

fn run_stage(args: RunArgs) -> Result<ExitCode> {
    let config = load_config(args.config.as_deref())?;
    let poly_bin = std::env::current_exe().context("failed to resolve the running poly binary")?;
    let stage = resolve_run_stage(args.stage.as_deref(), &config.hooks)?;

    let message_file = resolve_message_file(stage, args.message_file)?;
    let root = poly_hooks::git::get_root().context("failed to resolve the git repository root")?;
    let sources = super::sources::provision(&root, &config.hooks, false, false).context("provisioning hook sources")?;
    let files = candidate_files(&root, stage, args.all_files, None, None)?;
    let cache = open_result_cache(&config, &root, args.no_cache)?;
    let mut spec = lower::lower_stage(
        &config.hooks,
        &poly_bin,
        stage,
        &files,
        &config.cache.results.hooks,
        &root,
        &config.tools,
    )?;
    super::sources::merge_stage(
        &mut spec,
        &sources,
        &poly_bin,
        &files,
        &config.cache.results.hooks,
        &root,
    )?;

    let snapshot = maybe_staged_snapshot(isolation_active(&config.hooks, args.all_files, stage), &spec, &root)?;
    let work_root = snapshot.as_ref().map(|snapshot| snapshot.path().to_path_buf());

    let request = poly_hooks::HookRunRequest {
        root,
        work_root,
        files,
        message_file,
        stages: vec![spec],
        concurrency: args.jobs,
        cache,
        sccache: sccache_settings(&config, args.no_sccache)?,
        progress: show_progress(),
    };
    run_and_report(request)
}

/// Whether to stream live per-hook progress: on when stderr is a terminal, so
/// interactive runs show which tool is running while captured logs stay quiet.
pub(crate) fn show_progress() -> bool {
    std::io::stderr().is_terminal()
}

/// A message-file stage run directly needs an explicit `--message-file`;
/// without it the stage would silently match no files and skip every hook.
fn resolve_message_file(stage: poly_hooks::Stage, provided: Option<PathBuf>) -> Result<Option<PathBuf>> {
    if matches!(RunInputMode::from(stage), RunInputMode::MessageFile) && provided.is_none() {
        anyhow::bail!(
            "the `{stage}` stage needs a commit-message file; pass `--message-file <path>`, \
             or let an installed git hook invoke `poly hooks hook-impl`"
        );
    }
    Ok(provided)
}

/// Resolve the requested stage: an explicit argument (alias-aware), else the
/// first configured default `stages`, else `pre-commit`.
fn resolve_run_stage(requested: Option<&str>, hooks: &poly_config::HooksConfig) -> Result<poly_hooks::Stage> {
    let config_stage = match requested {
        Some(name) => name
            .parse::<ConfigStage>()
            .with_context(|| format!("invalid stage `{name}`"))?,
        None => match hooks.stages.first() {
            Some(name) => name
                .parse::<ConfigStage>()
                .with_context(|| format!("invalid configured stage `{name}`"))?,
            None => ConfigStage::PreCommit,
        },
    };
    lower::to_hook_stage(config_stage).context(
        "the `always` pseudo-stage cannot be run directly; \
         its jobs are appended to every concrete stage",
    )
}

fn install(args: InstallArgs) -> Result<ExitCode> {
    let root = poly_hooks::git::get_root().context("failed to resolve the git repository root")?;
    let config = load_config(None)?;
    super::sources::provision(&root, &config.hooks, false, true).context("provisioning hook sources")?;
    let hooks_dir = poly_hooks::git::get_git_hooks_dir().context("failed to resolve the git hooks directory")?;
    // An explicit `--hook-type` list is honoured verbatim; otherwise install
    // only the shims the config actually binds work to, rather than all ten
    // (which fire — and provision, and print — on every git operation).
    let derived = args.hook_types.is_empty();
    let hook_types = if derived {
        configured_hook_types(&config)
    } else {
        args.hook_types.clone()
    };
    let written = poly_hooks::install::install(&hooks_dir, &hook_types, args.overwrite)?;
    print_hook_summary("Installed", "install", &hooks_dir, &written);

    // When the shim set is derived from config, prune any leftover poly shims for
    // stages the config no longer binds — so a repo that previously installed all
    // ten hooks (an earlier poly, or a since-narrowed config) is realigned and
    // stops firing poly on every unrelated git operation. Foreign hooks and any
    // preserved `.legacy` are left untouched by `uninstall`. Skipped for an
    // explicit `--hook-type` list, which is treated as additive.
    if derived {
        let kept: BTreeSet<&str> = hook_types.iter().map(|hook_type| hook_type.as_ref()).collect();
        let stale: Vec<poly_hooks::HookType> = poly_hooks::HookType::value_variants()
            .iter()
            .copied()
            .filter(|hook_type| !kept.contains(hook_type.as_ref()))
            .collect();
        let removed = poly_hooks::install::uninstall(&hooks_dir, &stale)?;
        if !removed.is_empty() {
            print_hook_summary("Removed stale", "remove", &hooks_dir, &removed);
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// The [`poly_hooks::HookType`] a runner [`poly_hooks::Stage`] installs a shim
/// for, or `None` for [`poly_hooks::Stage::Manual`] (run on demand via `poly
/// hooks run`, never wired to a git hook).
fn hook_type_of_stage(stage: poly_hooks::Stage) -> Option<poly_hooks::HookType> {
    use poly_hooks::{HookType, Stage};
    Some(match stage {
        Stage::CommitMsg => HookType::CommitMsg,
        Stage::PostCheckout => HookType::PostCheckout,
        Stage::PostCommit => HookType::PostCommit,
        Stage::PostMerge => HookType::PostMerge,
        Stage::PostRewrite => HookType::PostRewrite,
        Stage::PreCommit => HookType::PreCommit,
        Stage::PreMergeCommit => HookType::PreMergeCommit,
        Stage::PrePush => HookType::PrePush,
        Stage::PreRebase => HookType::PreRebase,
        Stage::PrepareCommitMsg => HookType::PrepareCommitMsg,
        // `Manual` is not a git hook; `Stage` is `#[non_exhaustive]`.
        _ => return None,
    })
}

/// Insert the concrete runner stage(s) a config stage lowers to into `set`.
///
/// [`ConfigStage::Always`] has no runner counterpart — its jobs append to every
/// concrete stage — so it expands to all git-triggered stages.
fn add_config_stage(set: &mut BTreeSet<poly_hooks::Stage>, stage: ConfigStage) {
    match lower::to_hook_stage(stage) {
        Some(concrete) => {
            set.insert(concrete);
        }
        None => {
            for hook_type in poly_hooks::HookType::value_variants() {
                set.insert(poly_hooks::Stage::from(*hook_type));
            }
        }
    }
}

/// Insert the concrete runner stages named by a raw `stages = [...]` list.
///
/// Unparseable stage names are ignored: they cannot map to any hook, and config
/// validation reports genuine errors on its own path.
fn add_string_stages(set: &mut BTreeSet<poly_hooks::Stage>, stages: &[String]) {
    for name in stages {
        if let Ok(stage) = name.parse::<ConfigStage>() {
            add_config_stage(set, stage);
        }
    }
}

/// Derive the concrete git hook types a configured repository binds work to, so
/// `poly hooks install` (without an explicit `--hook-type`) wires only those
/// shims instead of all ten.
///
/// Unions every stage the config references: the top-level `[hooks] stages`
/// list, each builtin group's `stages`, every enabled `[tools.<name>]` binding,
/// and every explicit `[hooks.<stage>]` table. The `always` pseudo-stage expands
/// to every concrete stage. Falls back to `pre-commit` when the config binds
/// nothing (e.g. a repo with no `[hooks]` section, or builtins that inherit the
/// default stage).
///
/// Producer hooks from `[[hooks.sources]]` declare their stages in the provisioned
/// catalog manifest (a runtime detail, not in `poly.toml`); they are not folded in
/// here, so a source-only repo installs at least the `pre-commit` fallback.
fn configured_hook_types(config: &PolyConfig) -> Vec<poly_hooks::HookType> {
    let hooks = &config.hooks;
    let mut set: BTreeSet<poly_hooks::Stage> = BTreeSet::new();

    add_string_stages(&mut set, &hooks.stages);

    add_string_stages(&mut set, &hooks.builtin.lint.stages);
    add_string_stages(&mut set, &hooks.builtin.fmt.stages);
    add_string_stages(&mut set, &hooks.builtin.commit.stages);
    add_string_stages(&mut set, &hooks.builtin.file_safety.stages);
    if let Some(cargo) = &hooks.builtin.cargo {
        add_string_stages(&mut set, &cargo.stages);
    }

    for stage in hooks.stage_configs.keys() {
        add_config_stage(&mut set, *stage);
    }

    for (_, tool) in config.tools.iter() {
        if tool.enabled {
            for stage in &tool.stages {
                add_config_stage(&mut set, *stage);
            }
        }
    }

    let types: Vec<poly_hooks::HookType> = set.into_iter().filter_map(hook_type_of_stage).collect();
    if types.is_empty() {
        vec![poly_hooks::HookType::PreCommit]
    } else {
        types
    }
}

fn update_sources() -> Result<ExitCode> {
    let root = poly_hooks::git::get_root().context("failed to resolve the git repository root")?;
    let config = load_config(None)?;
    super::sources::provision(&root, &config.hooks, true, false).context("updating hook sources")?;
    let source_count = config.hooks.sources.len();
    println!(
        "Updated {} hook source{}.",
        source_count,
        if source_count == 1 { "" } else { "s" }
    );
    Ok(ExitCode::SUCCESS)
}

fn uninstall(args: UninstallArgs) -> Result<ExitCode> {
    let hooks_dir = poly_hooks::git::get_git_hooks_dir().context("failed to resolve the git hooks directory")?;
    let removed = poly_hooks::install::uninstall(&hooks_dir, &args.hook_types)?;
    print_hook_summary("Removed", "remove", &hooks_dir, &removed);
    Ok(ExitCode::SUCCESS)
}

/// Render a colored summary of installed/removed hook shims.
///
/// `done` is the past-tense header verb ("Installed" / "Removed"); `verb` the
/// bare infinitive used in the empty-set notice ("install" / "remove"). Paths
/// are shown relative to the current directory (the hooks live under a single
/// directory, printed once) so no absolute paths appear in the output.
fn print_hook_summary(done: &str, verb: &str, hooks_dir: &Path, hooks: &[PathBuf]) {
    if hooks.is_empty() {
        println!(
            "{} no poly git hooks to {verb}.",
            "·".if_supports_color(Stdout, |t| t.dimmed())
        );
        return;
    }
    let dir = relative_to_cwd(hooks_dir);
    let plural = if hooks.len() == 1 { "" } else { "s" };
    println!(
        "{} {done} {} git hook{plural} in {}",
        "✓".if_supports_color(Stdout, |t| t.green()),
        hooks.len().if_supports_color(Stdout, |t| t.bold()),
        dir.display().if_supports_color(Stdout, |t| t.cyan()),
    );
    for path in hooks {
        let name = path.file_name().map_or_else(|| path.as_os_str(), |n| n);
        println!(
            "  {} {}",
            "›".if_supports_color(Stdout, |t| t.dimmed()),
            name.to_string_lossy()
        );
    }
}

/// Strip the current-directory prefix from `path` so the display is relative;
/// falls back to `path` unchanged when it is not under the cwd (or the cwd is
/// unavailable) or is already relative.
fn relative_to_cwd(path: &Path) -> PathBuf {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| path.strip_prefix(&cwd).ok().map(Path::to_path_buf))
        .unwrap_or_else(|| path.to_path_buf())
}

fn hook_impl(args: HookImplArgs) -> Result<ExitCode> {
    let root = poly_hooks::git::get_root().context("failed to resolve the git repository root")?;
    let config = load_config(args.config.as_deref())?;
    let sources = super::sources::provision(&root, &config.hooks, false, false).context("provisioning hook sources")?;
    let Some(inputs) = poly_hooks::hook_impl::hook_impl(args.hook_type, &args.git_args, &root)? else {
        return Ok(ExitCode::SUCCESS);
    };

    let poly_bin = std::env::current_exe().context("failed to resolve the running poly binary")?;
    let stage = inputs.stage;
    let files = candidate_files(
        &root,
        stage,
        inputs.all_files,
        inputs.from_ref.as_deref(),
        inputs.to_ref.as_deref(),
    )?;
    let cache = open_result_cache(&config, &root, args.no_cache)?;
    let mut spec = lower::lower_stage(
        &config.hooks,
        &poly_bin,
        stage,
        &files,
        &config.cache.results.hooks,
        &root,
        &config.tools,
    )?;
    super::sources::merge_stage(
        &mut spec,
        &sources,
        &poly_bin,
        &files,
        &config.cache.results.hooks,
        &root,
    )?;

    let snapshot = maybe_staged_snapshot(isolation_active(&config.hooks, inputs.all_files, stage), &spec, &root)?;
    let work_root = snapshot.as_ref().map(|snapshot| snapshot.path().to_path_buf());

    let request = poly_hooks::HookRunRequest {
        root,
        work_root,
        files,
        message_file: inputs.message_file,
        stages: vec![spec],
        concurrency: args.jobs,
        cache,
        sccache: sccache_settings(&config, args.no_sccache)?,
        progress: show_progress(),
    };
    run_and_report(request)
}

/// Resolve the candidate file set for `stage`.
///
/// File-mode stages get the staged files, the `from..to` diff range, or — with
/// `all_files` — the whole tracked tree. Message-file and no-file stages get an
/// empty set (the runner supplies the message file from the request).
fn candidate_files(
    root: &Path,
    stage: poly_hooks::Stage,
    all_files: bool,
    from_ref: Option<&str>,
    to_ref: Option<&str>,
) -> Result<Vec<PathBuf>> {
    match RunInputMode::from(stage) {
        RunInputMode::NoFiles | RunInputMode::MessageFile => Ok(Vec::new()),
        RunInputMode::Files => {
            if all_files {
                Ok(poly_hooks::git::list_files(root)?)
            } else if let (Some(from), Some(to)) = (from_ref, to_ref) {
                Ok(poly_hooks::git::get_changed_files(from, to, root)?)
            } else {
                Ok(poly_hooks::git::get_staged_files(root)?)
            }
        }
    }
}

/// Whether whole-workspace hooks should be isolated to staged content for this
/// run.
///
/// The snapshot is a copy of the git **index**, so isolation only makes sense
/// for the commit-gating stages (`pre-commit`, `pre-merge-commit`), where the
/// index *is* what will be committed. It is never applied to `--all-files`
/// (which intentionally checks the whole tree) or to non-index stages such as
/// `pre-push`. Within those bounds the `[hooks] isolate` override wins,
/// defaulting to on.
fn isolation_active(hooks: &poly_config::HooksConfig, all_files: bool, stage: poly_hooks::Stage) -> bool {
    let index_stage = matches!(stage, poly_hooks::Stage::PreCommit | poly_hooks::Stage::PreMergeCommit);
    index_stage && !all_files && hooks.isolate.unwrap_or(true)
}

/// Refresh the staged-content snapshot when isolation is active and `spec`
/// actually contains a whole-workspace hook — otherwise there is nothing to
/// isolate and the refresh is skipped.
fn maybe_staged_snapshot(isolate: bool, spec: &poly_hooks::StageSpec, root: &Path) -> Result<Option<StagedSnapshot>> {
    if !isolate || !spec.hooks.iter().any(|hook| hook.workspace) {
        return Ok(None);
    }
    let snapshot = StagedSnapshot::create(root).context("failed to create the staged-content snapshot")?;
    Ok(Some(snapshot))
}

/// Open the tier-1 result cache for a hook run, honouring `[cache] enabled`,
/// the optional `[cache] dir` override, and the `--no-cache` flag.
///
/// Returns `None` when caching is disabled — the runner then neither reads nor
/// writes cache entries.
pub(crate) fn open_result_cache(config: &PolyConfig, root: &Path, no_cache: bool) -> Result<Option<ResultCache>> {
    let enabled = config.cache.enabled && !no_cache;
    let cache = match &config.cache.dir {
        Some(dir) => ResultCache::open(PathBuf::from(dir), enabled),
        None => ResultCache::open_from(root, enabled),
    }
    .context("failed to open the hook result cache")?;
    Ok(enabled.then_some(cache))
}

/// Resolve tier-2 sccache settings for a hook run from the `[cache.sccache]`
/// table, honouring the `--no-sccache` flag.
///
/// Returns `None` (sccache off) unless `[cache.sccache] enabled = true` and
/// `--no-sccache` was not given. The binary defaults to `"sccache"` when
/// `[cache.sccache] bin` is absent.
pub(crate) fn sccache_settings(config: &PolyConfig, no_sccache: bool) -> Result<Option<poly_hooks::SccacheSettings>> {
    let sccache = &config.cache.sccache;
    if !config.cache.enabled || !sccache.enabled || no_sccache {
        return Ok(None);
    }
    Ok(Some(poly_hooks::SccacheSettings {
        bin: sccache.validated_bin()?.to_string(),
        dir: sccache.dir.clone().map(PathBuf::from),
        max_size: sccache.max_size.clone(),
    }))
}

fn run_and_report(request: poly_hooks::HookRunRequest) -> Result<ExitCode> {
    let outcome = poly_hooks::run(request)?;
    let report = poly_hooks::HookRunReporter::new().render(&outcome);
    print!("{report}");
    Ok(if outcome.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

pub(crate) fn load_config(explicit: Option<&Path>) -> Result<PolyConfig> {
    match explicit {
        Some(path) => PolyConfig::load_file(path),
        None => {
            let cwd = std::env::current_dir().context("failed to resolve the working directory")?;
            PolyConfig::load(&cwd)
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::ValueEnum as _;

    use super::*;

    /// Load a [`PolyConfig`] from an inline `poly.toml` fixture.
    fn config_from(toml: &str) -> PolyConfig {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("poly.toml");
        std::fs::write(&path, toml).expect("write poly.toml");
        PolyConfig::load_file(&path).expect("parse poly.toml")
    }

    /// The kebab-case names of the derived hook types, sorted for comparison.
    fn derived_names(config: &PolyConfig) -> Vec<String> {
        let mut names: Vec<String> = configured_hook_types(config)
            .iter()
            .map(|hook_type| hook_type.as_ref().to_string())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn derives_only_the_configured_stages_not_all_ten() {
        let config = config_from(
            r#"
[hooks]
stages = ["pre-commit"]
[hooks.builtin]
commit = { stages = ["commit-msg"] }
"#,
        );
        assert_eq!(derived_names(&config), vec!["commit-msg", "pre-commit"]);
    }

    #[test]
    fn empty_config_falls_back_to_pre_commit() {
        let config = config_from("");
        assert_eq!(derived_names(&config), vec!["pre-commit"]);
    }

    #[test]
    fn always_pseudo_stage_expands_to_every_concrete_stage() {
        let config = config_from(
            r#"
[hooks.always]
[[hooks.always.jobs]]
run = "echo everywhere"
"#,
        );
        assert_eq!(
            derived_names(&config).len(),
            poly_hooks::HookType::value_variants().len(),
            "`always` must expand to every git-triggered hook type"
        );
    }

    #[test]
    fn builtin_and_stage_table_and_tool_stages_all_contribute() {
        let config = config_from(
            r#"
[hooks]
stages = ["pre-commit"]
[hooks.builtin]
fmt = { stages = ["pre-push"] }
[hooks.pre-merge-commit]
[[hooks.pre-merge-commit.jobs]]
run = "echo merge"
[tools.shfmt]
enabled = true
stages = ["post-checkout"]
"#,
        );
        assert_eq!(
            derived_names(&config),
            vec!["post-checkout", "pre-commit", "pre-merge-commit", "pre-push"]
        );
    }

    #[test]
    fn disabled_tool_stage_is_ignored() {
        let config = config_from(
            r#"
[hooks]
stages = ["pre-commit"]
[tools.shfmt]
enabled = false
stages = ["pre-push"]
"#,
        );
        assert_eq!(derived_names(&config), vec!["pre-commit"]);
    }
}
