//! `poly` — the single universal, zero-dependency linter & formatter CLI.
//!
//! `poly lint [PATHS]…` lints; `poly fmt [PATHS]…` formats; `poly commit`
//! lints/cleans a commit message (gitfluff). The same engine powers lint/fmt;
//! `polylint` and `polyfmt` ship as thin aliases for those two subcommands.

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use poly_cli::{CacheArgs, FmtArgs, HooksArgs, LintArgs, run_cache, run_fmt, run_hooks, run_lint};

#[derive(Parser)]
#[command(
    name = "poly",
    version,
    about = "Universal, zero-dependency linter & formatter",
    propagate_version = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Lint files (report diagnostics; never writes).
    Lint(LintArgs),
    /// Format files (dry-run by default; use --fix to write in place).
    Fmt(FmtArgs),
    /// Lint and optionally clean a commit message (reads `[commit]` from poly.toml).
    Commit(Box<gitfluff::cli::LintArgs>),
    /// Run git hooks declared in `[hooks]` of poly.toml (native runner).
    Hooks(HooksArgs),
    /// Inspect and maintain the result cache (stats / size / gc / clean).
    Cache(CacheArgs),
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Lint(args) => run_lint(args),
        Command::Fmt(args) => run_fmt(args),
        Command::Commit(args) => run_commit(*args),
        Command::Hooks(args) => run_hooks(args),
        Command::Cache(args) => run_cache(args),
    }
}

/// Run the gitfluff-backed commit-message linter and map its exit code onto an
/// [`ExitCode`].
fn run_commit(args: gitfluff::cli::LintArgs) -> ExitCode {
    match gitfluff::run_lint(args) {
        Ok(0) => ExitCode::SUCCESS,
        Ok(code) => ExitCode::from(code as u8),
        Err(error) => {
            eprintln!("poly commit: {error:#}");
            ExitCode::FAILURE
        }
    }
}
