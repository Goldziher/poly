//! `poly` — the single universal, zero-dependency linter & formatter CLI.
//!
//! `poly lint [PATHS]…` lints; `poly fmt [PATHS]…` formats. The same engine
//! powers both; `polylint` and `polyfmt` ship as thin aliases for the two
//! subcommands.

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use poly_cli::{FmtArgs, LintArgs, run_fmt, run_lint};

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
    /// Format files (writes in place; use --check for a dry run).
    Fmt(FmtArgs),
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Lint(args) => run_lint(args),
        Command::Fmt(args) => run_fmt(args),
    }
}
