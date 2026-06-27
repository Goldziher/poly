//! sqruff backend: SQL lint + format via [`sqruff_lib`].
//!
//! Dialect defaults to `ansi`. Override with `dialect = "postgres"` (or any other
//! dialect sqruff supports) in the `[lint.sql.sqruff]` / `[fmt.sql.sqruff]` config
//! table. Line length defaults to the polylint global (120).

use std::str::FromStr as _;

use sqruff_lib::core::config::{FluffConfig, Value};
use sqruff_lib::core::linter::core::Linter;
use sqruff_lib_core::dialects::init::DialectKind;
use sqruff_lib_core::errors::SQLBaseError;

use crate::config::EngineConfig;
use crate::engine::{Capabilities, Diagnostic, Engine, FormatOutput, Severity, SourceFile, Span};
use crate::language::Language;

/// sqruff SQL backend — lint + format for SQL files.
pub struct SqruffEngine;

/// sqruff-lib crate version; part of the cache key so upgrades invalidate stale results.
const SQRUFF_VERSION: &str = "0.38.0";

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
        Capabilities {
            lint: true,
            format: true,
            fix: true,
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
            if fixed == src.content {
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
/// Layering: sqruff defaults (from embedded `default_config.cfg`) → opinionated
/// polylint override (line length 120) → user `polylint.toml` options (dialect).
fn build_fluff_config(cfg: &EngineConfig) -> anyhow::Result<FluffConfig> {
    let mut fluff = FluffConfig::default();

    // Opinionated override: line length 120 (polylint global default).
    let line_length = cfg.globals.line_length as i32;
    if let Some(core) = fluff.raw.get_mut("core").and_then(|v| v.as_map_mut()) {
        core.insert("max_line_length".to_string(), Value::Int(line_length));
    }

    // User dialect override — defaults to ansi (sqruff's own default).
    let dialect_str = cfg
        .options
        .get("dialect")
        .and_then(|v| v.as_str())
        .unwrap_or("ansi");
    if dialect_str != "ansi" {
        let kind = DialectKind::from_str(dialect_str).map_err(|_| {
            anyhow::anyhow!(
                "unknown SQL dialect {dialect_str:?}; \
                 supported values: ansi, bigquery, clickhouse, databricks, db2, duckdb, \
                 greenplum, mysql, oracle, postgres, redshift, snowflake, sparksql, \
                 sqlite, trino, tsql"
            )
        })?;
        fluff
            .override_dialect(kind)
            .map_err(|e| anyhow::anyhow!("sqruff override_dialect failed: {e}"))?;
    }

    Ok(fluff)
}

/// Convert a sqruff [`SQLBaseError`] to a polylint [`Diagnostic`].
fn violation_to_diagnostic(violation: SQLBaseError) -> Diagnostic {
    let code_str = violation.rule_code();
    Diagnostic {
        engine: "sqruff".to_string(),
        // Map the sentinel "????" (no rule attached, e.g. parse errors) to None.
        code: if code_str == "????" {
            None
        } else {
            Some(code_str.to_string())
        },
        severity: Severity::Warning,
        message: violation.description.clone(),
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
        fix: None,
    }
}
