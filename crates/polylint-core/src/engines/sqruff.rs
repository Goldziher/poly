//! sqruff backend: SQL lint + format via [`sqruff_lib`].
//!
//! Dialect defaults to `ansi`. Override with `dialect = "postgres"` (or any other
//! dialect sqruff supports) in the `[lint.sql.sqruff]` / `[fmt.sql.sqruff]` config
//! table. Line length defaults to the polylint global (120).
//!
//! ## Supported config keys
//! | Key | Type | Description |
//! |-----|------|-------------|
//! | `dialect` | string | SQL dialect (default `"ansi"`) |
//! | `rules` | string array | Allow-list of rule codes/groups |
//! | `exclude_rules` | string array | Deny-list of rule codes/groups |
//! | `rule_configs` | table | Per-rule parameter overrides (see below) |
//!
//! ### Per-rule parameters (`rule_configs`)
//! Map rule section names to inline tables of key/value pairs:
//!
//! ```toml
//! [lint.sql.sqruff.rule_configs]
//! "capitalisation.keywords" = { capitalisation_policy = "upper" }
//! "layout.long_lines"       = { ignore_comment_lines = true }
//! ```
//!
//! These forward directly into sqruff's `[sqruff:rules:<name>]` INI sections.
//! Non-scalar values (nested tables, arrays) within a rule entry are ignored.
//!
//! **Note on `rule_configs` vs `rules`**: `rules` is an array of rule *codes*
//! for allow-listing; `rule_configs` is a table of *per-rule parameters*.  They
//! are separate keys and can coexist.

use std::str::FromStr as _;

use sqruff_lib::core::config::FluffConfig;
use sqruff_lib::core::linter::core::Linter;
use sqruff_lib_core::dialects::init::DialectKind;
use sqruff_lib_core::errors::SQLBaseError;

use crate::config::EngineConfig;
use crate::engine::{Capabilities, Diagnostic, Engine, FormatOutput, Severity, SourceFile, Span};
use crate::language::Language;

/// sqruff SQL backend — lint + format for SQL files.
pub struct SqruffEngine;

/// sqruff-lib crate version; part of the cache key so upgrades invalidate stale results.
///
/// Bumped to `+rule-configs-2` because parse/lex errors now emit `Error` severity (not
/// `Warning`), so the same input can yield different diagnostic output for the same
/// sqruff-lib version.
const SQRUFF_VERSION: &str = "0.38.0+rule-configs-2";

/// Languages handled by this backend.
static LANGUAGES: &[Language] = &[Language::Sql];

impl Engine for SqruffEngine {
    fn name(&self) -> &'static str {
        "sqruff"
    }

    fn languages(&self) -> &'static [Language] {
        LANGUAGES
    }

    fn capabilities(&self) -> Capabilities {
        // `fix` is false: sqruff's autofix edits are not wired through the
        // polylint `Edit` path, so advertising fix=true would silently do nothing
        // under `poly lint --fix`.  The format path already applies structural
        // fixes via `lint_string(…, fix=true)` / `fix_string()`.
        Capabilities {
            lint: true,
            format: true,
            fix: false,
        }
    }

    fn version(&self) -> &str {
        SQRUFF_VERSION
    }

    fn lint(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        let fluff_cfg = build_fluff_config(cfg)?;
        let linter = Linter::new(fluff_cfg, None, None, false)
            .map_err(|e| anyhow::anyhow!("sqruff Linter::new failed: {e}"))?;
        let filename = src.path.to_string_lossy().into_owned();
        let linted = linter
            .lint_string(&src.content, Some(filename), false)
            .map_err(|e| anyhow::anyhow!("sqruff lint_string failed: {e}"))?;
        Ok(linted
            .into_violations()
            .into_iter()
            .map(violation_to_diagnostic)
            .collect())
    }

    fn format(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
        let fluff_cfg = build_fluff_config(cfg)?;
        let linter = Linter::new(fluff_cfg, None, None, false)
            .map_err(|e| anyhow::anyhow!("sqruff Linter::new failed: {e}"))?;
        let filename = src.path.to_string_lossy().into_owned();
        let linted = linter
            .lint_string(&src.content, Some(filename), true)
            .map_err(|e| anyhow::anyhow!("sqruff format lint_string failed: {e}"))?;
        if linted.has_fixes() {
            let fixed = linted.fix_string();
            if fixed == *src.content {
                Ok(FormatOutput::Unchanged)
            } else {
                Ok(FormatOutput::Formatted(fixed))
            }
        } else {
            Ok(FormatOutput::Unchanged)
        }
    }
}

/// Build a [`FluffConfig`] from a polylint [`EngineConfig`].
///
/// Constructs an INI-format config string from user options and passes it to
/// [`FluffConfig::from_source`], which merges the user overrides on top of
/// sqruff's own embedded `default_config.cfg`.
///
/// Layering: sqruff defaults → opinionated polylint override (`max_line_length`
/// 120) → user `polylint.toml` options.
fn build_fluff_config(cfg: &EngineConfig) -> anyhow::Result<FluffConfig> {
    let dialect_str = cfg
        .options
        .get("dialect")
        .and_then(|v| v.as_str())
        .unwrap_or("ansi");

    // Validate the dialect upfront so we can surface a descriptive error rather
    // than panicking inside FluffConfig::new.
    if dialect_str != "ansi" {
        DialectKind::from_str(dialect_str).map_err(|_| {
            anyhow::anyhow!(
                "unknown SQL dialect {dialect_str:?}; \
                 supported values: ansi, bigquery, clickhouse, databricks, db2, duckdb, \
                 greenplum, mysql, oracle, postgres, redshift, snowflake, sparksql, \
                 sqlite, trino, tsql"
            )
        })?;
    }

    // Build an INI config string.  FluffConfig::from_source merges this with
    // sqruff's built-in defaults — user entries win on conflict.
    let mut ini = format!("[sqruff]\nmax_line_length = {}\n", cfg.globals.line_length);

    if dialect_str != "ansi" {
        ini.push_str(&format!("dialect = {dialect_str}\n"));
    }

    // Rule allow-list: `rules = ["CP01", "LT01"]` selects only those rules.
    if let Some(rules) = cfg.options.get("rules").and_then(|v| v.as_array()) {
        let codes: Vec<&str> = rules.iter().filter_map(|v| v.as_str()).collect();
        if !codes.is_empty() {
            ini.push_str(&format!("rules = {}\n", codes.join(",")));
        }
    }

    // Rule deny-list: `exclude_rules = ["CP01"]` suppresses those rules.
    if let Some(excluded) = cfg.options.get("exclude_rules").and_then(|v| v.as_array()) {
        let codes: Vec<&str> = excluded.iter().filter_map(|v| v.as_str()).collect();
        if !codes.is_empty() {
            ini.push_str(&format!("exclude_rules = {}\n", codes.join(",")));
        }
    }

    // Per-rule parameters: `[lint.sql.sqruff.rule_configs]`.
    // Each key is a rule section name (e.g. `"capitalisation.keywords"`);
    // each value is an inline table of option key/value pairs.
    // These become `[sqruff:rules:<name>]` INI sections that sqruff merges on
    // top of its own rule defaults.
    if let Some(rule_configs) = cfg.options.get("rule_configs").and_then(|v| v.as_table()) {
        for (rule_name, rule_opts) in rule_configs {
            if let Some(opts_table) = rule_opts.as_table() {
                ini.push_str(&format!("\n[sqruff:rules:{rule_name}]\n"));
                for (key, val) in opts_table {
                    // Skip non-scalar values rather than emit a bare `key = `
                    // (which sqruff's INI parser would read as an empty value).
                    let val_str = toml_val_to_ini_str(val);
                    if !val_str.is_empty() {
                        ini.push_str(&format!("{key} = {val_str}\n"));
                    }
                }
            }
        }
    }

    Ok(FluffConfig::from_source(&ini, None))
}

/// Convert a scalar [`toml::Value`] into a bare string for an INI entry value.
///
/// Non-scalar values (arrays, tables) are rendered as an empty string — per-rule
/// parameters are expected to be scalars.  Booleans use sqruff's `True`/`False`
/// casing (case-insensitive in the INI parser, but matches the convention).
fn toml_val_to_ini_str(v: &toml::Value) -> String {
    match v {
        // Strip newlines so a value can't inject a spurious `[section]` or key
        // into the generated INI.
        toml::Value::String(s) => s.replace(['\n', '\r'], " "),
        toml::Value::Boolean(b) => {
            if *b {
                "True".to_string()
            } else {
                "False".to_string()
            }
        }
        toml::Value::Integer(i) => i.to_string(),
        toml::Value::Float(f) => f.to_string(),
        _ => String::new(),
    }
}

/// Convert a sqruff [`SQLBaseError`] to a polylint [`Diagnostic`].
///
/// Parse and lex errors carry the sentinel code `"????"` (sqruff's internal
/// "no rule attached" marker).  These are structural failures — the file could
/// not be parsed — and are mapped to [`Severity::Error`].  Real rule violations
/// (any other code) are [`Severity::Warning`].
fn violation_to_diagnostic(violation: SQLBaseError) -> Diagnostic {
    let code_str = violation.rule_code();
    let is_parse_error = code_str == "????";
    Diagnostic {
        engine: "sqruff".to_string(),
        // Map the sentinel "????" (no rule attached, e.g. parse/lex errors) to None.
        code: if is_parse_error {
            None
        } else {
            Some(code_str.to_string())
        },
        severity: if is_parse_error {
            Severity::Error
        } else {
            Severity::Warning
        },
        title: violation.description.clone(),
        description: None,
        url: None,
        span: if violation.line_no > 0 {
            Some(Span {
                start_line: violation.line_no as u32,
                start_col: violation.line_pos as u32,
                end_line: violation.line_no as u32,
                end_col: violation.line_pos as u32,
            })
        } else {
            None
        },
        fix: vec![],
        metadata: Default::default(),
    }
}
