//! `poly rules` — inspect and test the ast-grep rules a run would apply.
//!
//! - `poly rules test [DIR…]` — verify each rule against its `*-test.yml`
//!   snippets: `valid` snippets must not match, `invalid` snippets must. Exits
//!   non-zero on any failed snippet or a test naming an unknown rule.
//! - `poly rules list [DIR…]` — list every resolved rule (id, language,
//!   built-in or user, declared severity, and the severity it reports at under
//!   the current config).
//!
//! With no `DIR`, both read the `[rules] dirs` from the nearest `poly.toml`.
//! The inventory comes from `poly_host_services::ops::rules_report_for`: in-process when the
//! ast-grep engine family is linked, otherwise from the `astgrep` component (installed on demand
//! unless `--offline` or `[components] install = "never"`).
//! `list` covers poly's **built-in rule pack** as well as user rules, and
//! reflects the config that governs them: `[rules] builtin = false` removes the
//! pack, and `[lint.astgrep]` `select` / `extend_select` / `ignore` /
//! `[lint.astgrep.rules.<id>] level` move individual rules on, off, or to a
//! different severity.

use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use poly_core::{Config, report};
use poly_frontend_api::dto::{RuleInfo, RulesReport};
use poly_host_services::ops::rules_report_for;
use serde::Serialize;

use crate::OutputFormat;

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
    /// List every resolved rule — built-in pack and user rules alike.
    List(RulesListArgs),
}

/// Shared positional argument: which directories to search for rules.
#[derive(Args)]
pub struct RulesScope {
    /// Rule directories to search (default: `[rules] dirs` from poly.toml).
    #[arg(value_name = "DIR")]
    pub dirs: Vec<String>,
}

/// `poly rules list` arguments.
#[derive(Args)]
pub struct RulesListArgs {
    /// Rule directories to search (default: `[rules] dirs` from poly.toml).
    /// The built-in pack is listed regardless, unless `[rules] builtin = false`.
    #[arg(value_name = "DIR")]
    pub dirs: Vec<String>,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Pretty)]
    pub format: OutputFormat,
}

/// The machine-readable `poly rules list` document.
///
/// Carries exactly what the human table shows: the rule directories searched,
/// whether the built-in pack is enabled (which is *why* a listing may hold no
/// built-in rows), and one record per rule.
#[derive(Serialize)]
struct RulesListDocument<'a> {
    dirs: &'a [String],
    builtin_pack_enabled: bool,
    rules: &'a [RuleInfo],
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
        RulesCommand::Test(scope) => run_test(scope.dirs),
        RulesCommand::List(args) => run_list(args),
    }
}

/// The inventory for `dirs` (empty: `[rules] dirs`) under the nearest config.
fn report_for(dirs: &[String], test: bool) -> Result<RulesReport> {
    let cwd = std::env::current_dir().context("failed to resolve the working directory")?;
    let config = Config::load_with(&cwd, &crate::config_sources::resolver()?).context("failed to load config")?;
    rules_report_for(&config, dirs, test, None)
}

fn run_test(dirs: Vec<String>) -> Result<ExitCode> {
    let report = report_for(&dirs, true)?;
    let outcomes = report.tests.as_deref().unwrap_or_default();

    for outcome in outcomes.iter().filter(|outcome| !outcome.passed) {
        let expected = match outcome.kind.as_str() {
            "valid" => "no match",
            "invalid" => "a match",
            _ => "matching autofix output",
        };
        let reason = outcome.detail.as_deref().unwrap_or(expected);
        println!(
            "FAIL  {rule} [{kind} #{index}] — expected {reason}",
            rule = outcome.rule_id,
            kind = outcome.kind,
            index = outcome.index,
        );
    }
    for id in report.missing_rule_ids.iter().flatten() {
        println!("ERROR test references unknown rule id `{id}`");
    }
    for id in report.untested_rule_ids.iter().flatten() {
        println!("warn  rule `{id}` has no test file");
    }

    let passed = outcomes.iter().filter(|outcome| outcome.passed).count();
    println!(
        "\n{passed} passed, {failed} failed across {rules} in {dirs}",
        failed = outcomes.len() - passed,
        rules = poly_core::report::rules(report.total_rules.unwrap_or_default()),
        dirs = report.dirs.join(", "),
    );

    if report.ok == Some(true) {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(1))
    }
}

fn run_list(args: RulesListArgs) -> Result<ExitCode> {
    let report = report_for(&args.dirs, false)?;
    let document = RulesListDocument {
        dirs: &report.dirs,
        builtin_pack_enabled: report.builtin_pack_enabled,
        rules: &report.rules,
    };

    let rendered = match args.format {
        OutputFormat::Pretty => {
            print_pretty(&document);
            return Ok(ExitCode::SUCCESS);
        }
        OutputFormat::Json => report::render_json(&document),
        OutputFormat::Toon => report::render_toon(&document),
    };
    match crate::emit_structured(rendered) {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(code) => Ok(ExitCode::from(code)),
    }
}

/// Render the human table: one row per rule, then a summary naming where the
/// rules came from and how many are actually active.
fn print_pretty(document: &RulesListDocument<'_>) {
    let dirs = if document.dirs.is_empty() {
        "<no rule dirs configured>".to_string()
    } else {
        document.dirs.join(", ")
    };

    if document.rules.is_empty() {
        println!("no rules found in: {dirs}");
        if !document.builtin_pack_enabled {
            println!("note: the built-in rule pack is disabled (`[rules] builtin = false`)");
        }
        return;
    }

    let id_width = document
        .rules
        .iter()
        .map(|rule| rule.id.len())
        .max()
        .unwrap_or(2)
        .max(2);
    let lang_width = document
        .rules
        .iter()
        .map(|rule| rule.language.len())
        .max()
        .unwrap_or(8)
        .max(8);

    println!(
        "{id:<id_width$}  {lang:<lang_width$}  {source:<7}  {severity:<8}  DEFAULT",
        id = "ID",
        lang = "LANGUAGE",
        source = "SOURCE",
        severity = "SEVERITY",
    );
    for rule in document.rules {
        println!(
            "{id:<id_width$}  {lang:<lang_width$}  {source:<7}  {severity:<8}  {default}",
            id = rule.id,
            lang = rule.language,
            source = rule.source,
            severity = rule.severity,
            default = rule.default_severity,
        );
    }

    let builtin = document.rules.iter().filter(|rule| rule.source == "builtin").count();
    let enabled = document.rules.iter().filter(|rule| rule.enabled).count();
    println!(
        "\n{total}: {builtin} built-in, {user} user (from {dirs}); {enabled} enabled",
        total = poly_core::report::rules(document.rules.len()),
        user = document.rules.len() - builtin,
    );
    if !document.builtin_pack_enabled {
        println!("note: the built-in rule pack is disabled (`[rules] builtin = false`)");
    }
    println!("SEVERITY is what a finding reports at under this config; DEFAULT is the rule's own declared level.");
}
