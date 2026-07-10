//! `poly rules` — inspect and test custom ast-grep rule packs.
//!
//! - `poly rules test [DIR…]` — verify each rule against its `*-test.yml`
//!   snippets: `valid` snippets must not match, `invalid` snippets must. Exits
//!   non-zero on any failed snippet or a test naming an unknown rule.
//! - `poly rules list [DIR…]` — list discovered rules (id, language, severity).
//!
//! With no `DIR`, both read the `[rules] dirs` from the nearest `poly.toml`.

use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use poly_config::PolyConfig;
use poly_core::engines::astgrep::rules::load_flat;
use poly_core::engines::astgrep::test::{CaseKind, run_tests};

/// `poly rules` arguments.
#[derive(Args)]
pub struct RulesArgs {
    /// The rules operation to perform.
    #[command(subcommand)]
    pub command: RulesCommand,
}

/// The `poly rules` subcommands.
#[derive(Subcommand)]
pub enum RulesCommand {
    /// Verify custom rules against their `*-test.yml` snippets.
    Test(RulesScope),
    /// List discovered custom rules (id, language, severity).
    List(RulesScope),
}

/// Shared positional argument: which directories to search for rules.
#[derive(Args)]
pub struct RulesScope {
    /// Rule directories to search (default: `[rules] dirs` from poly.toml).
    #[arg(value_name = "DIR")]
    pub dirs: Vec<String>,
}

/// Run `poly rules`, mapping any error to exit code 2.
pub fn run_rules(args: RulesArgs) -> ExitCode {
    match run(args) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("poly rules: {error:#}");
            ExitCode::from(2)
        }
    }
}

fn run(args: RulesArgs) -> Result<ExitCode> {
    match args.command {
        RulesCommand::Test(scope) => run_test(resolve_dirs(scope.dirs)?),
        RulesCommand::List(scope) => run_list(resolve_dirs(scope.dirs)?),
    }
}

/// Use the given dirs, or fall back to `[rules] dirs` from the nearest config.
fn resolve_dirs(dirs: Vec<String>) -> Result<Vec<String>> {
    if !dirs.is_empty() {
        return Ok(dirs);
    }
    let cwd = std::env::current_dir().context("failed to resolve the working directory")?;
    let config = PolyConfig::load(&cwd).context("failed to load config")?;
    Ok(config.rules.dirs)
}

fn run_test(dirs: Vec<String>) -> Result<ExitCode> {
    let report = run_tests(&dirs)?;

    for outcome in &report.outcomes {
        if outcome.passed {
            continue;
        }
        let (kind, expected) = match outcome.kind {
            CaseKind::Valid => ("valid", "no match"),
            CaseKind::Invalid => ("invalid", "a match"),
            CaseKind::Fixed => ("fixed", "matching autofix output"),
        };
        let reason = outcome.detail.as_deref().unwrap_or(expected);
        println!(
            "FAIL  {rule} [{kind} #{index}] — expected {reason}",
            rule = outcome.rule_id,
            index = outcome.index,
        );
    }
    for id in &report.missing_rule_ids {
        println!("ERROR test references unknown rule id `{id}`");
    }
    for id in &report.untested_rule_ids {
        println!("warn  rule `{id}` has no test file");
    }

    println!(
        "\n{passed} passed, {failed} failed across {rules} rule(s) in {dirs}",
        passed = report.passed(),
        failed = report.failed(),
        rules = report.total_rules,
        dirs = dirs.join(", "),
    );

    if report.is_ok() {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(1))
    }
}

fn run_list(dirs: Vec<String>) -> Result<ExitCode> {
    let rules = load_flat(&dirs)?;
    if rules.is_empty() {
        println!("no rules found in: {}", dirs.join(", "));
        return Ok(ExitCode::SUCCESS);
    }
    for rule in &rules {
        println!(
            "{id:<24} {lang:<12} {severity:?}",
            id = rule.id,
            lang = rule.language.name(),
            severity = rule.severity,
        );
    }
    println!("\n{} rule(s) in {}", rules.len(), dirs.join(", "));
    Ok(ExitCode::SUCCESS)
}
