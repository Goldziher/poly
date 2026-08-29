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
//! `list` covers poly's **built-in rule pack** as well as user rules, and
//! reflects the config that governs them: `[rules] builtin = false` removes the
//! pack, and `[lint.astgrep]` `select` / `extend_select` / `ignore` /
//! `[lint.astgrep.rules.<id>] level` move individual rules on, off, or to a
//! different severity.

use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use poly_config::PolyConfig;
use poly_core::engines::astgrep::test::{CaseKind, run_tests};
use poly_core::engines::astgrep::{RuleListing, list_rules};
use poly_core::{Config, Kind, Language, report};
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
    rules: &'a [RuleListing],
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
        RulesCommand::List(args) => run_list(args),
    }
}

/// Use the given dirs, or fall back to `[rules] dirs` from the nearest config.
fn resolve_dirs(dirs: Vec<String>) -> Result<Vec<String>> {
    if !dirs.is_empty() {
        return Ok(dirs);
    }
    let cwd = std::env::current_dir().context("failed to resolve the working directory")?;
    let config = PolyConfig::load_with(&cwd, &crate::config_sources::resolver()?).context("failed to load config")?;
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

/// Resolve what `poly rules list` should list: the user rule dirs (explicit
/// argument, else `[rules] dirs`) and the resolved `astgrep` engine config that
/// governs the built-in pack and rule selection.
fn listing_inputs(dirs: Vec<String>) -> Result<(Vec<String>, bool, poly_core::config::EngineConfig)> {
    let cwd = std::env::current_dir().context("failed to resolve the working directory")?;
    let config = Config::load_with(&cwd, &crate::config_sources::resolver()?).context("failed to load config")?;
    // `astgrep`'s options table is language-agnostic — `Config::engine_config`
    // builds it from `[rules]` + `[lint.astgrep]`, never from a per-language
    // table — so the language passed here only has to be *some* language.
    let engine_config = config.engine_config(&Language::Other("astgrep".to_string()), "astgrep", Kind::Lint);
    let dirs = if dirs.is_empty() {
        config.rules_dirs.clone()
    } else {
        dirs
    };
    Ok((dirs, config.rules_builtin_pack, engine_config))
}

fn run_list(args: RulesListArgs) -> Result<ExitCode> {
    let (dirs, builtin_pack_enabled, engine_config) = listing_inputs(args.dirs)?;
    let rules = list_rules(&dirs, &engine_config)?;
    let document = RulesListDocument {
        dirs: &dirs,
        builtin_pack_enabled,
        rules: &rules,
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
            source = rule.source.as_str(),
            severity = rule.severity,
            default = rule.default_severity,
        );
    }

    let builtin = document
        .rules
        .iter()
        .filter(|rule| rule.source == poly_core::engines::astgrep::RuleSource::Builtin)
        .count();
    let enabled = document.rules.iter().filter(|rule| rule.enabled).count();
    println!(
        "\n{total} rule(s): {builtin} built-in, {user} user (from {dirs}); {enabled} enabled",
        total = document.rules.len(),
        user = document.rules.len() - builtin,
    );
    if !document.builtin_pack_enabled {
        println!("note: the built-in rule pack is disabled (`[rules] builtin = false`)");
    }
    println!("SEVERITY is what a finding reports at under this config; DEFAULT is the rule's own declared level.");
}
