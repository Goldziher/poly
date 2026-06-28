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

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use poly_cache::ResultCache;
use poly_config::{PolyConfig, Stage as ConfigStage};
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

    /// Path to the config file (default: nearest poly.toml / polylint.toml).
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
    /// Hook types to install (default: every git-triggered hook type).
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

    /// Path to the config file (default: nearest poly.toml / polylint.toml).
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

// ── run ───────────────────────────────────────────────────────────────────────

fn run_stage(args: RunArgs) -> Result<ExitCode> {
    let config = load_config(args.config.as_deref())?;
    let poly_bin = std::env::current_exe().context("failed to resolve the running poly binary")?;
    let stage = resolve_run_stage(args.stage.as_deref(), &config.hooks)?;

    let message_file = resolve_message_file(stage, args.message_file)?;
    let root = poly_hooks::git::get_root().context("failed to resolve the git repository root")?;
    let files = candidate_files(&root, stage, args.all_files, None, None)?;
    let cache = open_result_cache(&config, &root, args.no_cache)?;
    let spec = lower::lower_stage(
        &config.hooks,
        &poly_bin,
        stage,
        &files,
        &config.cache.results.hooks,
        &root,
    )?;

    let request = poly_hooks::HookRunRequest {
        root,
        files,
        message_file,
        stages: vec![spec],
        concurrency: args.jobs,
        cache,
        sccache: sccache_settings(&config, args.no_sccache),
    };
    run_and_report(request)
}

/// A message-file stage run directly needs an explicit `--message-file`;
/// without it the stage would silently match no files and skip every hook.
fn resolve_message_file(
    stage: poly_hooks::Stage,
    provided: Option<PathBuf>,
) -> Result<Option<PathBuf>> {
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
fn resolve_run_stage(
    requested: Option<&str>,
    hooks: &poly_config::HooksConfig,
) -> Result<poly_hooks::Stage> {
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

// ── install / uninstall ─────────────────────────────────────────────────────

fn install(args: InstallArgs) -> Result<ExitCode> {
    let hooks_dir = poly_hooks::git::get_git_hooks_dir()
        .context("failed to resolve the git hooks directory")?;
    let poly_bin = std::env::current_exe().context("failed to resolve the running poly binary")?;
    let written =
        poly_hooks::install::install(&hooks_dir, &poly_bin, &args.hook_types, args.overwrite)?;
    for path in &written {
        println!("installed {}", path.display());
    }
    Ok(ExitCode::SUCCESS)
}

fn uninstall(args: UninstallArgs) -> Result<ExitCode> {
    let hooks_dir = poly_hooks::git::get_git_hooks_dir()
        .context("failed to resolve the git hooks directory")?;
    let removed = poly_hooks::install::uninstall(&hooks_dir, &args.hook_types)?;
    for path in &removed {
        println!("uninstalled {}", path.display());
    }
    Ok(ExitCode::SUCCESS)
}

// ── hook-impl ─────────────────────────────────────────────────────────────────

fn hook_impl(args: HookImplArgs) -> Result<ExitCode> {
    let root = poly_hooks::git::get_root().context("failed to resolve the git repository root")?;
    let Some(inputs) = poly_hooks::hook_impl::hook_impl(args.hook_type, &args.git_args, &root)?
    else {
        // Nothing to do (e.g. a `pre-push` with nothing to push).
        return Ok(ExitCode::SUCCESS);
    };

    let config = load_config(args.config.as_deref())?;
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
    let spec = lower::lower_stage(
        &config.hooks,
        &poly_bin,
        stage,
        &files,
        &config.cache.results.hooks,
        &root,
    )?;

    let request = poly_hooks::HookRunRequest {
        root,
        files,
        message_file: inputs.message_file,
        stages: vec![spec],
        concurrency: args.jobs,
        cache,
        sccache: sccache_settings(&config, args.no_sccache),
    };
    run_and_report(request)
}

// ── shared helpers ──────────────────────────────────────────────────────────

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

/// Open the tier-1 result cache for a hook run, honouring `[cache] enabled`,
/// the optional `[cache] dir` override, and the `--no-cache` flag.
///
/// Returns `None` when caching is disabled — the runner then neither reads nor
/// writes cache entries.
fn open_result_cache(
    config: &PolyConfig,
    root: &Path,
    no_cache: bool,
) -> Result<Option<ResultCache>> {
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
fn sccache_settings(config: &PolyConfig, no_sccache: bool) -> Option<poly_hooks::SccacheSettings> {
    let sccache = &config.cache.sccache;
    // The master `[cache] enabled` flag is a global kill switch; sccache is a
    // further opt-in layered on top of it.
    if !config.cache.enabled || !sccache.enabled || no_sccache {
        return None;
    }
    Some(poly_hooks::SccacheSettings {
        bin: sccache.bin.clone().unwrap_or_else(|| "sccache".to_string()),
        dir: sccache.dir.clone().map(PathBuf::from),
        max_size: sccache.max_size.clone(),
    })
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

fn load_config(explicit: Option<&Path>) -> Result<PolyConfig> {
    match explicit {
        Some(path) => PolyConfig::load_file(path),
        None => {
            let cwd = std::env::current_dir().context("failed to resolve the working directory")?;
            PolyConfig::load(&cwd)
        }
    }
}
