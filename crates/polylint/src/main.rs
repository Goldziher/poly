//! `polylint` — universal, zero-dependency linter CLI.
//!
//! Thin alias for `poly lint`; the shared implementation lives in `poly-cli`.

use std::process::ExitCode;

use clap::Parser;
use poly_cli::{LintArgs, run_lint};

#[derive(Parser)]
#[command(
    name = "polylint",
    version,
    about = "Universal, zero-dependency linter (alias for `poly lint`)"
)]
struct Cli {
    #[command(flatten)]
    args: LintArgs,
}

fn main() -> ExitCode {
    poly_cli::init_logging();
    run_lint(Cli::parse().args)
}
