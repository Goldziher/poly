//! Human rendering of the [`DoctorReport`].
//!
//! The layout answers, in order, the questions a misled consumer actually has:
//! *what ran*, *what would run if I typed `poly`*, *what config did it read*,
//! and *what is wrong*.
//!
//! Coloring goes through owo-colors' `if_supports_color`, matching the rest of
//! poly's human output, so `--no-color`, a pipe, and `NO_COLOR` all strip it.

use poly_core::report::theme::Theme;

use super::report::{DoctorReport, Finding, PathReport, Severity};

/// The palette, resolved against stdout — the stream `poly doctor` prints to.
const THEME: Theme = Theme::STDOUT;

/// Print the report in the human format.
pub fn print_pretty(report: &DoctorReport) {
    print_running(report);
    print_path(&report.path);
    print_config(report);
    print_findings(&report.findings);
}

/// `1 install` / `2 installs`, grouped like every other poly count.
fn installs(count: usize) -> String {
    poly_core::report::quantity(count, "install", "installs")
}

/// Section heading.
fn heading(text: &str) -> String {
    THEME.heading(text)
}

fn print_running(report: &DoctorReport) {
    let running = &report.running;
    println!("{}", heading("running executable"));
    let executable = running
        .executable
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "<could not be determined>".to_string());
    println!("  path      {executable}");
    println!("  version   {}", running.version);
    let channel = match running.channel {
        "release" => THEME.success(running.channel),
        "dev" => THEME.skipped(running.channel),
        _ => THEME.failure(running.channel),
    };
    println!(
        "  build     {} ({channel}, {} profile)",
        running.build_id, running.profile
    );
    if let Some(commit) = running.commit {
        println!("  commit    {commit}");
    }
    println!();
}

fn print_path(path: &PathReport) {
    println!("{}", heading("poly on PATH"));
    if path.installs.is_empty() {
        println!("  (none — `poly` does not resolve by name)");
        println!();
        return;
    }
    for install in &path.installs {
        let marker = if install.running { "->" } else { "  " };
        let version = if install.probe.is_failure() {
            THEME.failure(install.probe.display())
        } else {
            install.probe.display().to_string()
        };
        println!("  {marker} {}. {}", install.order, install.path.display());
        if let Some(resolved) = &install.resolved {
            println!("        -> {}", resolved.display());
        }
        println!("        {version}");
    }
    if path.shadowing > 0 {
        println!(
            "  {} {} earlier on PATH than the running one",
            THEME.failure("shadowed:"),
            installs(path.shadowing)
        );
    }
    if path.shadowed > 0 {
        println!(
            "  {} the running executable hides {} other",
            THEME.failure("shadowing:"),
            installs(path.shadowed)
        );
    }
    if !path.running_on_path {
        println!("  note: the running executable was invoked by path, not through PATH");
    }
    println!();
}

fn print_config(report: &DoctorReport) {
    println!("{}", heading("config"));
    match &report.config.path {
        Some(path) => println!("  file      {}", path.display()),
        None => println!("  file      (none found)"),
    }
    if let Some(local) = &report.config.local_override {
        println!("  override  {}", local.display());
    }
    if let Some(lock) = &report.config.lock {
        println!("  lock      {}", lock.display());
    }
    match &report.config.error {
        Some(error) => println!("  status    {} {error}", THEME.failure("failed to load:")),
        None => println!("  status    loaded"),
    }
    match (&report.cache.directory, &report.cache.error) {
        (Some(directory), _) => println!("  cache     {}", directory.display()),
        (None, Some(error)) => println!("  cache     {} {error}", THEME.skipped("unresolved:")),
        (None, None) => println!("  cache     (unresolved)"),
    }
    println!();
}

fn print_findings(findings: &[Finding]) {
    if findings.is_empty() {
        println!("{} no problems found", THEME.success("ok:"));
        return;
    }
    println!("{}", heading("findings"));
    for finding in findings {
        let label = match finding.severity {
            Severity::Error => THEME.failure("error"),
            Severity::Warning => THEME.skipped("warning"),
            Severity::Note => "note".to_string(),
        };
        println!("  {label}: {}", finding.summary);
        if let Some(remedy) = &finding.remedy {
            println!("    fix: {remedy}");
        }
    }
}

/// Render the report as JSON for a bug report or a CI check.
pub fn to_json(report: &DoctorReport) -> String {
    serde_json::to_string_pretty(report).unwrap_or_else(|error| format!("{{\"error\":\"{error}\"}}"))
}
