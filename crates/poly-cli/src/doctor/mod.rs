//! `poly doctor` — the command you run before filing a bug.
//!
//! Every incident this exists for had the same shape: **the report of success
//! and the effect disagreed**. `brew upgrade poly` said it upgraded while
//! `poly --version` never moved; an MCP server kept answering from a binary
//! deleted hours earlier; a build reported a released version number while
//! carrying eight unreleased fixes; a binary with an invalidated code signature
//! exited silently in a way indistinguishable from a hang.
//!
//! What they have in common is that no single command would tell you *which
//! poly you were actually talking to*. `poly doctor` is that command: it prints
//! the running executable and its build identity, every `poly` on `PATH` with
//! the version each one reports, the config in effect, and the cache directory —
//! then exits non-zero when it finds something actively wrong.
//!
//! [`excludes`] extends that to the same disagreement one level up: a run whose
//! `[discovery] exclude` quietly hides more of the tree than its author meant
//! also reports success, over files it never looked at.

pub mod excludes;
pub mod probe;
pub mod render;
pub mod report;
pub mod shadow;

use std::process::ExitCode;

use clap::Args;

pub use report::{DoctorReport, Finding, Severity};
pub use shadow::warn_if_conflicting;

/// Output format for `poly doctor`.
#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum DoctorFormat {
    /// Human-oriented, colored output.
    Pretty,
    /// JSON — paste it into a bug report, or assert on it in CI.
    Json,
    /// TOON (Token-Oriented Object Notation), the same document as `json`.
    Toon,
}

/// `poly doctor` arguments.
#[derive(Args)]
pub struct DoctorArgs {
    /// Output format.
    #[arg(long, value_enum, default_value_t = DoctorFormat::Pretty)]
    pub format: DoctorFormat,
}

/// Run `poly doctor`.
///
/// Exits `1` when any finding is error-severity — a competing install on
/// `PATH`, a `poly` that cannot report its own version, or a config that does
/// not load. Notes (a development build, an executable invoked by path) do not
/// fail the run.
pub fn run_doctor(args: DoctorArgs) -> ExitCode {
    let flags = crate::global();
    match flags.color {
        crate::ColorChoice::Never => owo_colors::set_override(false),
        crate::ColorChoice::Always => owo_colors::set_override(true),
        crate::ColorChoice::Auto => {}
    }
    let report = report::collect(flags.config.as_deref());
    match args.format {
        DoctorFormat::Pretty => render::print_pretty(&report),
        DoctorFormat::Json => println!("{}", render::to_json(&report)),
        DoctorFormat::Toon => match poly_core::report::render_toon(&report) {
            Ok(document) => println!("{document}"),
            Err(error) => {
                eprintln!("error: {error}");
                return ExitCode::from(2);
            }
        },
    }
    if report.has_errors() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
