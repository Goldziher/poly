//! The `poly` command-line entry point.
//!
//! `poly lint [PATHS]…` lints; `poly fmt [PATHS]…` formats; `poly commit`
//! lints/cleans a commit message (gitfluff). The same engine powers lint/fmt.
//! [`main`] is the whole program, so a second binary can register engine providers
//! and then call it.

use std::ffi::OsString;
use std::process::ExitCode;

use crate::{
    CacheArgs, ComponentsArgs, ConfigArgs, DoctorArgs, FmtArgs, GlobalArgs, HooksArgs, LintArgs, LockArgs, MigrateArgs,
    RulesArgs, run_cache, run_components, run_config, run_doctor, run_fmt, run_hooks, run_lint, run_lock, run_migrate,
    run_rules,
};
use clap::{Parser, Subcommand};

/// The `doctor` subcommand, which reports PATH conflicts itself and so is
/// exempt from the one-line warning.
const DOCTOR_COMMAND: &str = "doctor";

#[derive(Parser)]
#[command(
    name = "poly",
    version = poly_buildinfo::long_version(),
    about = "polylint: one linter & formatter for every language in your repository",
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
    /// are not poly's to control. Pass --workspace never, or set `[workspace] run =
    /// "never"`, for a run that leaves the tree untouched.
    Lint(LintArgs),
    /// Format files (dry-run by default; use --fix to write in place).
    Fmt(FmtArgs),
    /// Lint and optionally clean a commit message (reads `[commit]` from poly.toml).
    Commit(Box<crate::CommitArgs>),
    /// Run git hooks declared in `[hooks]` of poly.toml (native runner).
    Hooks(HooksArgs),
    /// Absorb foreign tool configs into poly.toml and remove what poly can honor.
    Migrate(MigrateArgs),
    /// Inspect and maintain the result cache (stats / gc / clean).
    Cache(CacheArgs),
    /// Inspect and test custom ast-grep rule packs (test / list).
    Rules(RulesArgs),
    /// Inspect and manage downloadable engine components (list / status / install / sync / prune).
    #[command(alias = "component")]
    Components(ComponentsArgs),
    /// Pin remote `extends` bases and Git hook sources in `poly.lock`.
    Lock(LockArgs),
    /// Inspect the effective config and print the `poly.toml` JSON Schema.
    Config(ConfigArgs),
    /// Run an MCP server over stdio (mirrors the CLI).
    Mcp(McpArgs),
    /// Report which poly is running, every poly on PATH, and the config in effect.
    Doctor(DoctorArgs),
}

/// Arguments for `poly mcp`. The server is the `mcp` component, fetched on first use, unless this
/// build links it in (`bundled-mcp`). It reads `poly.toml` per request like the
/// CLI; the global `--config` pins a fallback config file for requests that
/// don't name one.
#[derive(clap::Args)]
struct McpArgs {}

/// Exit status of a process whose reader went away, as a shell reports death by SIGPIPE.
const EXIT_BROKEN_PIPE: i32 = 141;

/// How std's `failed printing to stdout` panic names a reader that went away: the Unix text, and the
/// Windows codes `ERROR_BROKEN_PIPE` (109, "The pipe has been ended") and `ERROR_NO_DATA` (232).
const BROKEN_PIPE_MARKERS: &[&str] = &["Broken pipe", "(os error 109)", "(os error 232)"];

/// Make a closed stdout (`poly config schema | head`) end the run quietly.
///
/// `println!` panics when its reader is gone, and the default hook prints a backtrace-style message for
/// what is the normal way a pager or `head` stops reading. Every command that prints shares this path, so
/// one hook covers them all.
fn exit_quietly_on_broken_pipe() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info.payload();
        let message = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&str>().copied())
            .unwrap_or_default();
        if message.contains("failed printing to std") && BROKEN_PIPE_MARKERS.iter().any(|m| message.contains(m)) {
            std::process::exit(EXIT_BROKEN_PIPE);
        }
        default_hook(info);
    }));
}

/// Parses the process arguments and runs the requested `poly` subcommand.
///
/// Call after any `poly_core::register_engine_provider` registration.
pub fn main() -> ExitCode {
    exit_quietly_on_broken_pipe();
    let arguments: Vec<OsString> = std::env::args_os().collect();
    // Emitted before clap parses, so `poly --version` — the command that
    // started two false bug reports against already-fixed code — is covered
    // too: clap prints the version and exits without ever reaching a
    // subcommand handler. `poly doctor` is exempt because it reports the same
    // conflict in full, with versions and a remedy.
    if !names_doctor(&arguments) {
        crate::doctor::warn_if_conflicting();
    }

    let cli = Cli::parse_from(arguments);
    let debug = cli.global.debug;
    let mcp_config = cli.global.config.clone();
    cli.global.install();
    match cli.command {
        Command::Lint(args) => run_lint(args),
        Command::Fmt(args) => run_fmt(args),
        Command::Commit(args) => {
            crate::init_logging_with(debug);
            run_commit(*args)
        }
        Command::Hooks(args) => {
            crate::init_logging_with(debug);
            run_hooks(args)
        }
        Command::Migrate(args) => {
            crate::init_logging_with(debug);
            run_migrate(args)
        }
        Command::Cache(args) => {
            crate::init_logging_with(debug);
            run_cache(args)
        }
        Command::Rules(args) => {
            crate::init_logging_with(debug);
            run_rules(args)
        }
        Command::Components(args) => {
            crate::init_logging_with(debug);
            run_components(args)
        }
        Command::Lock(args) => {
            crate::init_logging_with(debug);
            run_lock(args)
        }
        Command::Config(args) => {
            crate::init_logging_with(debug);
            run_config(args)
        }
        Command::Doctor(args) => {
            crate::init_logging_with(debug);
            run_doctor(args)
        }
        Command::Mcp(_args) => {
            crate::init_logging_with(debug);
            crate::run_mcp(mcp_config)
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
fn run_commit(args: crate::CommitArgs) -> ExitCode {
    // Resolve `poly.toml` `extends` bases (including pinned remote git bases) so a
    // repo whose shared config lives in a remote — not a local sibling path —
    // still loads its `[commit]` rules for message linting.
    let resolver = match crate::config_sources::resolver() {
        Ok(resolver) => resolver,
        Err(error) => {
            eprintln!("poly commit: {error:#}");
            return ExitCode::FAILURE;
        }
    };
    let mut args = args;
    args.color = crate::global().color;
    match crate::run_commit_lint(args, &resolver) {
        Ok(0) => ExitCode::SUCCESS,
        Ok(code) => ExitCode::from(code as u8),
        Err(error) => {
            eprintln!("poly commit: {error:#}");
            ExitCode::FAILURE
        }
    }
}
