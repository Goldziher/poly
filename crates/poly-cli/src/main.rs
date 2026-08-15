//! `poly` — the single universal, zero-dependency linter & formatter CLI.
//!
//! `poly lint [PATHS]…` lints; `poly fmt [PATHS]…` formats; `poly commit`
//! lints/cleans a commit message (gitfluff). The same engine powers lint/fmt.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use poly_cli::{
    CacheArgs, ConfigArgs, DoctorArgs, FmtArgs, HooksArgs, LintArgs, MigrateArgs, RulesArgs, run_cache, run_config,
    run_doctor, run_fmt, run_hooks, run_lint, run_migrate, run_rules,
};

/// The `doctor` subcommand, which reports PATH conflicts itself and so is
/// exempt from the one-line warning.
const DOCTOR_COMMAND: &str = "doctor";

#[derive(Parser)]
#[command(
    name = "poly",
    version = poly_buildinfo::long_version(),
    about = "Universal, zero-dependency linter & formatter",
    propagate_version = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Lint files (poly applies no fixes without --fix).
    ///
    /// Not read-only, though: the whole-project phase executes the configured
    /// whole-project tools (cargo clippy and friends) against the live worktree
    /// even without --fix. poly asks them for check mode, but their own side
    /// effects — a refreshed lock file, a populated build or type-checker cache —
    /// are not poly's to control. Pass --no-workspace, or set `[lint] workspace =
    /// false`, for a run that leaves the tree untouched.
    Lint(LintArgs),
    /// Format files (dry-run by default; use --fix to write in place).
    Fmt(FmtArgs),
    /// Lint and optionally clean a commit message (reads `[commit]` from poly.toml).
    Commit(Box<gitfluff::cli::LintArgs>),
    /// Run git hooks declared in `[hooks]` of poly.toml (native runner).
    Hooks(HooksArgs),
    /// Absorb foreign tool configs into poly.toml and remove what poly can honor.
    Migrate(MigrateArgs),
    /// Inspect and maintain the result cache (stats / size / gc / clean).
    Cache(CacheArgs),
    /// Inspect and test custom ast-grep rule packs (test / list).
    Rules(RulesArgs),
    /// Manage shared configuration: lock remote `extends` bases and inspect the effective config.
    Config(ConfigArgs),
    /// Run an MCP server over stdio (mirrors the CLI).
    Mcp(McpArgs),
    /// Report which poly is running, every poly on PATH, and the config in effect.
    Doctor(DoctorArgs),
}

/// Arguments for `poly mcp`. The server reads `poly.toml` per request like the
/// CLI; `--config` pins a fallback config file for requests that don't name one.
#[derive(Args)]
struct McpArgs {
    /// Path to a config file used for requests that do not specify their own.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
}

fn main() -> ExitCode {
    let arguments: Vec<OsString> = std::env::args_os().collect();
    // Emitted before clap parses, so `poly --version` — the command that
    // started two false bug reports against already-fixed code — is covered
    // too: clap prints the version and exits without ever reaching a
    // subcommand handler. `poly doctor` is exempt because it reports the same
    // conflict in full, with versions and a remedy.
    if !names_doctor(&arguments) {
        poly_cli::doctor::warn_if_conflicting();
    }

    match Cli::parse_from(arguments).command {
        Command::Lint(args) => run_lint(args),
        Command::Fmt(args) => run_fmt(args),
        Command::Commit(args) => {
            poly_cli::init_logging();
            run_commit(*args)
        }
        Command::Hooks(args) => {
            poly_cli::init_logging();
            run_hooks(args)
        }
        Command::Migrate(args) => {
            poly_cli::init_logging();
            run_migrate(args)
        }
        Command::Cache(args) => {
            poly_cli::init_logging();
            run_cache(args)
        }
        Command::Rules(args) => {
            poly_cli::init_logging();
            run_rules(args)
        }
        Command::Config(args) => {
            poly_cli::init_logging();
            run_config(args)
        }
        Command::Doctor(args) => {
            poly_cli::init_logging();
            run_doctor(args)
        }
        Command::Mcp(args) => {
            poly_cli::init_logging();
            match poly_mcp::serve(args.config) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("poly mcp: {error:#}");
                    ExitCode::FAILURE
                }
            }
        }
    }
}

/// Whether the argument list invokes `poly doctor`.
///
/// Matches the first argument that is not a flag, so `poly --no-color doctor`
/// is recognized as well as `poly doctor`.
fn names_doctor(arguments: &[OsString]) -> bool {
    arguments
        .iter()
        .skip(1)
        .find(|argument| !argument.to_string_lossy().starts_with('-'))
        .is_some_and(|argument| argument == DOCTOR_COMMAND)
}

/// Run the gitfluff-backed commit-message linter and map its exit code onto an
/// [`ExitCode`].
fn run_commit(args: gitfluff::cli::LintArgs) -> ExitCode {
    // Resolve `poly.toml` `extends` bases (including pinned remote git bases) so a
    // repo whose shared config lives in a remote — not a local sibling path —
    // still loads its `[commit]` rules for message linting.
    let resolver = match poly_cli::config_sources::resolver() {
        Ok(resolver) => resolver,
        Err(error) => {
            eprintln!("poly commit: {error:#}");
            return ExitCode::FAILURE;
        }
    };
    match gitfluff::run_lint_with_resolver(args, &resolver) {
        Ok(0) => ExitCode::SUCCESS,
        Ok(code) => ExitCode::from(code as u8),
        Err(error) => {
            eprintln!("poly commit: {error:#}");
            ExitCode::FAILURE
        }
    }
}
