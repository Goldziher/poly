//! `poly` — the single universal, zero-dependency linter & formatter CLI.
//!
//! `poly lint [PATHS]…` lints; `poly fmt [PATHS]…` formats; `poly commit`
//! lints/cleans a commit message (gitfluff). The same engine powers lint/fmt.

use std::ffi::OsString;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use poly_cli::{
    CacheArgs, ComponentsArgs, ConfigArgs, DoctorArgs, FmtArgs, GlobalArgs, HooksArgs, LintArgs, LockArgs, MigrateArgs,
    run_cache, run_components, run_config, run_doctor, run_fmt, run_hooks, run_lint, run_lock, run_migrate,
};
#[cfg(feature = "engine-astgrep")]
use poly_cli::{RulesArgs, run_rules};

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
    /// Flags every subcommand accepts.
    #[command(flatten)]
    global: GlobalArgs,

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
    /// are not poly's to control. Pass --workspace never, or set `[lint] workspace =
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
    /// Inspect and maintain the result cache (stats / gc / clean).
    Cache(CacheArgs),
    /// Inspect and test custom ast-grep rule packs (test / list).
    #[cfg(feature = "engine-astgrep")]
    Rules(RulesArgs),
    /// Inspect and manage downloadable engine components (list / status / install / sync / prune).
    #[command(alias = "component")]
    Components(ComponentsArgs),
    /// Pin remote `extends` bases and Git hook sources in `poly.lock`.
    Lock(LockArgs),
    /// Inspect the effective config and print the `poly.toml` JSON Schema.
    Config(ConfigArgs),
    /// Run an MCP server over stdio (mirrors the CLI).
    #[cfg(feature = "mcp")]
    Mcp(McpArgs),
    /// Report which poly is running, every poly on PATH, and the config in effect.
    Doctor(DoctorArgs),
}

/// Arguments for `poly mcp`. The server reads `poly.toml` per request like the
/// CLI; the global `--config` pins a fallback config file for requests that
/// don't name one.
#[cfg(feature = "mcp")]
#[derive(clap::Args)]
struct McpArgs {}

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

    let cli = Cli::parse_from(arguments);
    let debug = cli.global.debug;
    #[cfg(feature = "mcp")]
    let mcp_config = cli.global.config.clone();
    cli.global.install();
    match cli.command {
        Command::Lint(args) => run_lint(args),
        Command::Fmt(args) => run_fmt(args),
        Command::Commit(args) => {
            poly_cli::init_logging_with(debug);
            run_commit(*args)
        }
        Command::Hooks(args) => {
            poly_cli::init_logging_with(debug);
            run_hooks(args)
        }
        Command::Migrate(args) => {
            poly_cli::init_logging_with(debug);
            run_migrate(args)
        }
        Command::Cache(args) => {
            poly_cli::init_logging_with(debug);
            run_cache(args)
        }
        #[cfg(feature = "engine-astgrep")]
        Command::Rules(args) => {
            poly_cli::init_logging_with(debug);
            run_rules(args)
        }
        Command::Components(args) => {
            poly_cli::init_logging_with(debug);
            run_components(args)
        }
        Command::Lock(args) => {
            poly_cli::init_logging_with(debug);
            run_lock(args)
        }
        Command::Config(args) => {
            poly_cli::init_logging_with(debug);
            run_config(args)
        }
        Command::Doctor(args) => {
            poly_cli::init_logging_with(debug);
            run_doctor(args)
        }
        #[cfg(feature = "mcp")]
        Command::Mcp(_args) => {
            poly_cli::init_logging_with(debug);
            match poly_mcp::serve(mcp_config) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("poly mcp: {error:#}");
                    ExitCode::FAILURE
                }
            }
        }
    }
}

/// Global flags that take a separate value, which `names_doctor` must step over
/// so `poly --color never doctor` is not read as the subcommand `never`.
const VALUE_FLAGS: &[&str] = &["--color", "--config"];

/// Whether the argument list invokes `poly doctor`.
///
/// Matches the first argument that is not a flag (or a flag's value), so
/// `poly --color never doctor` is recognized as well as `poly doctor`.
fn names_doctor(arguments: &[OsString]) -> bool {
    let mut args = arguments.iter().skip(1).map(|argument| argument.to_string_lossy());
    while let Some(argument) = args.next() {
        if VALUE_FLAGS.contains(&argument.as_ref()) {
            args.next();
        } else if !argument.starts_with('-') {
            return argument == DOCTOR_COMMAND;
        }
    }
    false
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
    let mut args = args;
    args.color = match poly_cli::global().color {
        poly_cli::global::ColorChoice::Auto => gitfluff::cli::ColorMode::Auto,
        poly_cli::global::ColorChoice::Always => gitfluff::cli::ColorMode::Always,
        poly_cli::global::ColorChoice::Never => gitfluff::cli::ColorMode::Never,
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
