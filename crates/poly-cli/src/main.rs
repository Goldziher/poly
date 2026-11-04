//! `poly` — the single universal, zero-dependency linter & formatter CLI.

use std::process::ExitCode;

fn main() -> ExitCode {
    poly_cli::cli::main()
}
