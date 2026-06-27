//! Shared implementation behind the `poly` CLI and the `polylint` / `polyfmt`
//! alias binaries. The argument groups and run logic live here so all three
//! entry points stay in lock-step.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Args;
use polylint_core::{Config, RunOptions, report};

/// Output rendering format.
#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    /// Colored, human-oriented output.
    Pretty,
    /// JSON.
    Json,
    /// TOON (Token-Oriented Object Notation).
    Toon,
}

/// Flags shared by both subcommands.
#[derive(Args)]
pub struct CommonArgs {
    /// Files or directories to process (default: current directory).
    pub paths: Vec<PathBuf>,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Pretty)]
    pub format: OutputFormat,

    /// Path to a config file (default: nearest polylint.toml).
    #[arg(long)]
    pub config: Option<PathBuf>,

    /// Disable the result cache.
    #[arg(long)]
    pub no_cache: bool,

    /// Number of parallel jobs (default: all logical cores).
    #[arg(short = 'j', long)]
    pub jobs: Option<usize>,

    /// Disable colored output.
    #[arg(long)]
    pub no_color: bool,
}

/// `poly lint` arguments.
#[derive(Args)]
pub struct LintArgs {
    /// Flags shared with `poly fmt`.
    #[command(flatten)]
    pub common: CommonArgs,
}

/// `poly fmt` arguments.
#[derive(Args)]
pub struct FmtArgs {
    /// Check only: do not write; exit non-zero if any file would change.
    #[arg(long)]
    pub check: bool,

    /// Flags shared with `poly lint`.
    #[command(flatten)]
    pub common: CommonArgs,
}

/// Run the lint pipeline and map the outcome to a process exit code.
pub fn run_lint(args: LintArgs) -> ExitCode {
    let common = args.common;
    apply_color(&common);
    let (paths, config, opts) = match prepare(&common) {
        Ok(triple) => triple,
        Err(code) => return code,
    };

    match polylint_core::lint(&paths, &config, &opts) {
        Ok(results) => {
            let count = match common.format {
                OutputFormat::Pretty => report::report_lint_pretty(&results),
                OutputFormat::Json => {
                    println!("{}", report::report_lint_json(&results));
                    results.iter().map(|r| r.diagnostics.len()).sum()
                }
                OutputFormat::Toon => {
                    println!("{}", report::report_lint_toon(&results));
                    results.iter().map(|r| r.diagnostics.len()).sum()
                }
            };
            if count > 0 {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(2)
        }
    }
}

/// Run the format pipeline and map the outcome to a process exit code.
pub fn run_fmt(args: FmtArgs) -> ExitCode {
    let common = &args.common;
    apply_color(common);
    let (paths, config, opts) = match prepare(common) {
        Ok(triple) => triple,
        Err(code) => return code,
    };

    let write = !args.check;
    match polylint_core::format(&paths, &config, &opts, write) {
        Ok(results) => {
            let changed = match common.format {
                OutputFormat::Pretty => report::report_format_pretty(&results, args.check),
                OutputFormat::Json => {
                    println!("{}", report::report_format_json(&results));
                    results.iter().filter(|r| r.changed).count()
                }
                OutputFormat::Toon => {
                    println!("{}", report::report_format_toon(&results));
                    results.iter().filter(|r| r.changed).count()
                }
            };
            if args.check && changed > 0 {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(2)
        }
    }
}

fn apply_color(common: &CommonArgs) {
    if common.no_color {
        owo_colors::set_override(false);
    }
}

/// Resolve paths, load config, and build run options; on config failure return
/// the exit code to propagate.
fn prepare(common: &CommonArgs) -> Result<(Vec<PathBuf>, Config, RunOptions), ExitCode> {
    let paths = if common.paths.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        common.paths.clone()
    };
    let config = match load_config(common.config.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: failed to load config: {e:#}");
            return Err(ExitCode::from(2));
        }
    };
    let opts = RunOptions {
        no_cache: common.no_cache,
        jobs: common.jobs,
    };
    Ok((paths, config, opts))
}

fn load_config(explicit: Option<&Path>) -> anyhow::Result<Config> {
    match explicit {
        Some(p) => Config::load_file(p),
        None => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            Config::load(&cwd)
        }
    }
}
