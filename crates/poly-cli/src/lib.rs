//! Shared implementation behind the `poly` CLI.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Args;
use polylint_core::{Config, LintResult, RunOptions, Severity, Verbosity, report};

pub mod cache_cmd;
pub mod hooks;
pub mod migrate;
pub mod rules_cmd;

pub use cache_cmd::{CacheArgs, run_cache};
pub use hooks::{HooksArgs, run_hooks};
pub use migrate::{MigrateArgs, run_migrate};
pub use rules_cmd::{RulesArgs, run_rules};

/// Install the process-wide `tracing` subscriber for the CLI binaries at the
/// default verbosity (info-level poly notices). Equivalent to
/// [`init_logging_with(false)`](init_logging_with).
pub fn init_logging() {
    init_logging_with(false);
}

/// Install the process-wide `tracing` subscriber for the CLI binaries.
///
/// Idempotent (first call wins; safe to call from every entry point). Logs to
/// **stderr** so they never pollute `--format json` on stdout. The default
/// filter surfaces poly's own info-level notices — e.g. the "toolchain not
/// found; using the generic tier" fallback — while keeping dependencies quiet.
/// When `debug` is set, the poly crates are widened to `debug` level. `RUST_LOG`
/// always overrides either default.
///
/// Because the subscriber is first-call-wins, callers that honor `--debug` must
/// invoke this **after** argument parsing — see the binary entry points.
pub fn init_logging_with(debug: bool) {
    use tracing_subscriber::{EnvFilter, fmt};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        let directives = if debug {
            "warn,polylint_core=debug,poly_hooks=debug,poly_cache=debug,poly_cli=debug"
        } else {
            "warn,polylint_core=info,poly_hooks=info,poly_cache=info,poly_cli=info"
        };
        EnvFilter::new(directives)
    });
    let _ = fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .try_init();
}

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

    /// Gitignore-style glob to exclude from discovery (repeatable). Merged with
    /// the config's `[discovery] exclude`. Example: `--exclude 'test_apps/**'`.
    #[arg(long = "exclude", value_name = "GLOB")]
    pub exclude: Vec<String>,

    /// Disable colored output.
    #[arg(long)]
    pub no_color: bool,

    /// Apply fixes in place: autofixes for `lint`, formatting for `fmt`. The
    /// default is a dry run that reports what would change and writes nothing.
    #[arg(long)]
    pub fix: bool,

    /// Show extra per-finding detail in `pretty` output: description, rule URL,
    /// and metadata. No-op for `--format json`/`toon` (always fully structured).
    #[arg(long)]
    pub verbose: bool,

    /// Emit debug data: per-engine cache hit/miss and timing (shown in `pretty`,
    /// attached to `json`/`toon`), and raise log verbosity to `debug` on stderr.
    #[arg(long)]
    pub debug: bool,
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
    /// Explicit dry run (the default): report what would change, write nothing,
    /// exit non-zero if any file would change. Conflicts with `--fix`.
    #[arg(long, conflicts_with = "fix")]
    pub check: bool,

    /// Flags shared with `poly lint`.
    #[command(flatten)]
    pub common: CommonArgs,
}

/// Run the lint pipeline and map the outcome to a process exit code.
pub fn run_lint(args: LintArgs) -> ExitCode {
    let common = args.common;
    // Init logging after parse so `--debug` can widen the filter (first-call-wins).
    init_logging_with(common.debug);
    apply_color(&common);
    let verbosity = Verbosity::new(common.verbose, common.debug);
    let (paths, config, opts) = match prepare(&common) {
        Ok(triple) => triple,
        Err(code) => return code,
    };

    match polylint_core::lint(&paths, &config, &opts, common.fix, common.debug) {
        Ok(results) => {
            // Render (and print) all diagnostics regardless of severity; the count
            // returned here is not used for the exit decision.
            let _ = match common.format {
                OutputFormat::Pretty => report::report_lint_pretty(&results, verbosity),
                OutputFormat::Json => {
                    println!("{}", report::report_lint_json(&results));
                    results.iter().map(|r| r.diagnostics.len()).sum()
                }
                OutputFormat::Toon => {
                    println!("{}", report::report_lint_toon(&results));
                    results.iter().map(|r| r.diagnostics.len()).sum()
                }
            };
            // Follow the standard linter convention (ruff/eslint/clippy): only
            // error-severity findings fail the run. Warning/info/hint are
            // reported but non-blocking.
            lint_exit_code(&results)
        }
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(2)
        }
    }
}

/// Map lint results to a process exit code: `1` when any diagnostic has
/// [`Severity::Error`], `0` otherwise. Warning/info/hint findings are
/// non-blocking, matching the convention of ruff, eslint, and clippy.
fn lint_exit_code(results: &[LintResult]) -> ExitCode {
    if lint_has_errors(results) {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Whether any diagnostic across all results is error-severity.
fn lint_has_errors(results: &[LintResult]) -> bool {
    results
        .iter()
        .any(|r| r.diagnostics.iter().any(|d| d.severity == Severity::Error))
}

/// Run the format pipeline and map the outcome to a process exit code.
pub fn run_fmt(args: FmtArgs) -> ExitCode {
    let common = &args.common;
    // Init logging after parse so `--debug` can widen the filter (first-call-wins).
    init_logging_with(common.debug);
    apply_color(common);
    let verbosity = Verbosity::new(common.verbose, common.debug);
    let (paths, config, opts) = match prepare(common) {
        Ok(triple) => triple,
        Err(code) => return code,
    };

    // Dry run by default; `--fix` writes formatted output in place. `--check`
    // is an explicit alias for the default dry run.
    let write = common.fix;
    match polylint_core::format(&paths, &config, &opts, write, common.debug) {
        Ok(results) => {
            let changed = match common.format {
                OutputFormat::Pretty => report::report_format_pretty(&results, !write, verbosity),
                OutputFormat::Json => {
                    println!("{}", report::report_format_json(&results));
                    results.iter().filter(|r| r.changed).count()
                }
                OutputFormat::Toon => {
                    println!("{}", report::report_format_toon(&results));
                    results.iter().filter(|r| r.changed).count()
                }
            };
            if !write && changed > 0 {
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
        exclude: common.exclude.clone(),
        // An explicit `--config <path>` pins a single config and bypasses nested
        // (monorepo) resolution; without it, poly cascades nested `poly.toml`s.
        explicit_config: common.config.is_some(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use polylint_core::Diagnostic;

    fn diag(severity: Severity) -> Diagnostic {
        Diagnostic {
            engine: "test".to_string(),
            code: None,
            severity,
            title: "test finding".to_string(),
            description: None,
            span: None,
            url: None,
            fix: Vec::new(),
            metadata: std::collections::BTreeMap::new(),
        }
    }

    fn result(diagnostics: Vec<Diagnostic>) -> LintResult {
        LintResult {
            path: PathBuf::from("test.rs"),
            diagnostics,
            debug: None,
        }
    }

    #[test]
    fn no_diagnostics_yields_success() {
        assert!(!lint_has_errors(&[result(vec![])]));
    }

    #[test]
    fn warning_only_diagnostics_yield_success() {
        let results = vec![result(vec![
            diag(Severity::Warning),
            diag(Severity::Info),
            diag(Severity::Hint),
        ])];
        assert!(
            !lint_has_errors(&results),
            "warning/info/hint findings must not fail the run"
        );
    }

    #[test]
    fn error_diagnostic_yields_failure() {
        let results = vec![result(vec![diag(Severity::Warning), diag(Severity::Error)])];
        assert!(lint_has_errors(&results), "an error-severity finding must fail the run");
    }

    #[test]
    fn error_in_any_result_yields_failure() {
        let results = vec![
            result(vec![diag(Severity::Warning)]),
            result(vec![diag(Severity::Error)]),
        ];
        assert!(lint_has_errors(&results));
    }
}
