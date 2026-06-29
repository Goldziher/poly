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
    // `run_lint` initializes logging after parse so `--debug` can widen the
    // filter (the subscriber is first-call-wins).
    run_lint(Cli::parse().args)
}
