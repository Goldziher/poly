//! `polyfmt` — universal, zero-dependency formatter CLI.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use polylint_core::{Config, RunOptions, report};

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum OutputFormat {
    Human,
    Json,
}

#[derive(Parser)]
#[command(
    name = "polyfmt",
    version,
    about = "Universal, zero-dependency formatter"
)]
struct Cli {
    /// Files or directories to format (default: current directory).
    paths: Vec<PathBuf>,

    /// Check only: do not write; exit non-zero if any file would change.
    #[arg(long)]
    check: bool,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
    format: OutputFormat,

    /// Path to a config file (default: nearest polylint.toml).
    #[arg(long)]
    config: Option<PathBuf>,

    /// Disable the result cache.
    #[arg(long)]
    no_cache: bool,

    /// Number of parallel jobs (default: all logical cores).
    #[arg(short = 'j', long)]
    jobs: Option<usize>,

    /// Disable colored output.
    #[arg(long)]
    no_color: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if cli.no_color {
        owo_colors::set_override(false);
    }

    let paths = if cli.paths.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        cli.paths.clone()
    };

    let config = match load_config(cli.config.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: failed to load config: {e:#}");
            return ExitCode::from(2);
        }
    };

    let opts = RunOptions {
        no_cache: cli.no_cache,
        jobs: cli.jobs,
    };

    let write = !cli.check;
    match polylint_core::format(&paths, &config, &opts, write) {
        Ok(results) => {
            let changed = match cli.format {
                OutputFormat::Human => report::report_format_human(&results, cli.check),
                OutputFormat::Json => {
                    println!("{}", report::report_format_json(&results));
                    results.iter().filter(|r| r.changed).count()
                }
            };
            if cli.check && changed > 0 {
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

fn load_config(explicit: Option<&std::path::Path>) -> anyhow::Result<Config> {
    match explicit {
        Some(p) => Config::load_file(p),
        None => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            Config::load(&cwd)
        }
    }
}
