//! `polyfmt` — universal, zero-dependency formatter CLI.
//!
//! Thin alias for `poly fmt`; the shared implementation lives in `poly-cli`.

use std::process::ExitCode;

use clap::Parser;
use poly_cli::{FmtArgs, run_fmt};

#[derive(Parser)]
#[command(
    name = "polyfmt",
    version,
    about = "Universal, zero-dependency formatter (alias for `poly fmt`)"
)]
struct Cli {
    #[command(flatten)]
    args: FmtArgs,
}

fn main() -> ExitCode {
    // `run_fmt` initializes logging after parse so `--debug` can widen the
    // filter (the subscriber is first-call-wins).
    run_fmt(Cli::parse().args)
}
