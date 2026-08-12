//! sqruff backend: SQL lint + format via [`sqruff_lib`].
//!
//! Dialect defaults to `ansi`. Override with `dialect = "postgres"` (or any other
//! dialect sqruff supports) in the `[lint.sql.sqruff]` / `[fmt.sql.sqruff]` config
//! table. Line length defaults to the poly global (120).
//!
//! ## Supported config keys
//! | Key | Type | Description |
//! |-----|------|-------------|
//! | `dialect` | string | SQL dialect (default `"ansi"`) |
//! | `select` | string array | Allow-list of rule codes/groups (canonical, ADR 0016) |
//! | `rules` | string array | Allow-list alias for `select` (sqruff-native) |
//! | `ignore` | string array | Deny-list of rule codes/groups (canonical, ADR 0016) |
//! | `exclude_rules` | string array | Deny-list alias for `ignore` (sqruff-native) |
//! | `rule_configs` | table | Per-rule parameter overrides (see below) |
//!
//! The canonical and native keys are unioned when both are present. Blank codes
//! are surfaced with a `tracing::warn` and skipped rather than forwarded.
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
//!
//! ## Lint / format partition
//!
//! `lint` and `format` run **disjoint** rule sets, split on *presentation* vs
//! *meaning*: `poly fmt` applies only the rules that change how SQL looks, and
//! `poly lint` reports only the rules that touch what it does. See the
//! `FORMAT_OWNED_GROUPS` constant for the reasoning.

use std::str::FromStr as _;
use std::sync::LazyLock;

use sqruff_lib::core::config::FluffConfig;
use sqruff_lib::core::linter::core::Linter;
use sqruff_lib::core::rules::RuleGroups;
use sqruff_lib_core::dialects::init::DialectKind;
use sqruff_lib_core::errors::SQLBaseError;

use super::rule_config::{RuleSelection, string_list, union_codes, warn_and_skip_blank};
use crate::config::EngineConfig;
use crate::engine::{Capabilities, Diagnostic, Engine, FormatOutput, Severity, SourceFile, Span};
use crate::language::Language;

/// sqruff SQL backend — lint + format for SQL files.
pub struct SqruffEngine;

/// sqruff-lib crate version; part of the cache key so upgrades invalidate stale results.
///
/// `+rule-configs-2` marks parse/lex errors emitting `Error` severity (not `Warning`).
/// `-presentation-fmt` marks the lint/format partition below: `format` applies only
/// [`FORMAT_OWNED_GROUPS`] and `lint` reports only the complement, so the same input
/// yields different output for the same sqruff-lib version.
const SQRUFF_VERSION: &str = "0.39.0+rule-configs-2-presentation-fmt";

/// The rule groups `poly fmt` applies — and, being the format-owned half of the
/// partition, exactly the groups `poly lint` stays silent about.
///
/// The line is **presentation vs meaning**, not layout vs structure. A formatter
/// may change how a query *looks*; it may never change, or destroy the evidence
/// of, what the query *does*.
///
/// - `layout` (`LT01`–`LT15`) — spacing, indentation, commas, operators, line
///   length, blank lines, file start/end. Pure whitespace; the direct analogue of
///   rumdl's `RuleCategory::Whitespace`, which `engines/rumdl.rs` suppresses in
///   `lint` for the same reason.
/// - `capitalisation` (`CP01`–`CP05`) — keyword/identifier/function/literal/type
///   casing. `select` → `SELECT` is what every user expects a SQL formatter to
///   do, and casing is meaning-preserving: SQL keywords are case-insensitive, and
///   `CP02` only touches identifiers already resolved as unquoted (quoted
///   identifiers, whose case *is* significant, are `references.quoting`'s
///   business and stay on the lint side).
///
/// **Everything else is lint-owned and `poly fmt` never applies it**, because
/// every remaining group can change or obscure meaning:
///
/// - `convention` — `CV05` rewrites `WHERE id = NULL` into `WHERE id IS NULL`.
///   Those return *different rows*: `= NULL` is never true, `IS NULL` matches.
///   Silently applying it changes the result set and erases the evidence of a
///   real bug the author needs to see. `CV11` (casting style) and `CV07`
///   (bracket removal) are the same shape.
/// - `aliasing` — `AL05` deletes an alias it believes is unused; any blind spot
///   in its reference detection is a broken query. `AL07` rewrites every
///   reference to a table.
/// - `references` — `RF06` strips identifier quoting, which in PostgreSQL
///   case-folds the identifier and can repoint it at a different column.
/// - `ambiguous` — `AM02`/`AM03`/`AM05` make implicit set/sort/join semantics
///   explicit; that is an editorial choice about intent, not a rendering.
/// - `structure` — `ST05` turns a joined subquery into a CTE, `ST06` reorders
///   select targets, `ST07` rewrites `USING` into `ON`. Each has more than one
///   valid repair and picking one rewrites the author's query shape.
/// - `jinja` — `JJ01` pads `{{foo}}` to `{{ foo }}`, which *is* presentation, but
///   it is not named here so that any group not explicitly vetted lands on the
///   lint side. See [`FORMAT_SUPPRESSED_GROUPS`] for why that default is safe.
///
/// The findings above are all still *reported* by `poly lint` — nothing is
/// hidden, the author just decides the repair.
const FORMAT_OWNED_GROUPS: &[RuleGroups] = &[RuleGroups::Layout, RuleGroups::Capitalisation];

/// Group names `poly lint` must not run: the format-owned half, which `poly fmt`
/// already fixes silently.
static LINT_SUPPRESSED_GROUPS: LazyLock<Vec<&'static str>> =
    LazyLock::new(|| FORMAT_OWNED_GROUPS.iter().map(AsRef::as_ref).collect());

/// Group names `poly fmt` must not run: sqruff's whole taxonomy minus the
/// meta-groups (`all`, `core`, which every rule and every core rule carry) and
/// minus [`FORMAT_OWNED_GROUPS`].
///
/// Derived from sqruff's own registry rather than written out, so a group added
/// upstream is suppressed from `format` until someone explicitly vets it into
/// [`FORMAT_OWNED_GROUPS`]. That default fails in the safe direction: an
/// unreviewed rule can only cause `poly fmt` to do *less*, never to rewrite
/// meaning unreviewed.
///
/// Built once per process — `sqruff_lib::rules::rules()` instantiates the full
/// rule registry, which is far too expensive for the per-file path.
static FORMAT_SUPPRESSED_GROUPS: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    let mut names: Vec<&'static str> = sqruff_lib::rules::rules()
        .iter()
        .flat_map(|rule| rule.groups())
        .filter(|group| !matches!(group, RuleGroups::All | RuleGroups::Core))
        .filter(|group| !FORMAT_OWNED_GROUPS.contains(group))
        .map(AsRef::as_ref)
        .collect();
    names.sort_unstable();
    names.dedup();
    names
});

/// Which side of the lint/format partition a [`FluffConfig`] is being built for.
///
/// The two sides are disjoint by construction: each suppresses exactly the
/// groups the other owns.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    /// `poly lint` — everything except [`FORMAT_OWNED_GROUPS`].
    Lint,
    /// `poly fmt` — only [`FORMAT_OWNED_GROUPS`].
    Format,
}

impl Mode {
    /// The names of the rule groups this mode must not run.
    fn suppressed_groups(self) -> &'static [&'static str] {
        match self {
            Mode::Lint => &LINT_SUPPRESSED_GROUPS,
            Mode::Format => &FORMAT_SUPPRESSED_GROUPS,
        }
    }
}

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
            fix: false,
        }
    }

    fn version(&self) -> &str {
        SQRUFF_VERSION
    }

    fn lint(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        let fluff_cfg = build_fluff_config(cfg, Mode::Lint)?;
        let linter =
            Linter::new(fluff_cfg, None, None, false).map_err(|e| anyhow::anyhow!("sqruff Linter::new failed: {e}"))?;
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
        let fluff_cfg = build_fluff_config(cfg, Mode::Format)?;
        let linter =
            Linter::new(fluff_cfg, None, None, false).map_err(|e| anyhow::anyhow!("sqruff Linter::new failed: {e}"))?;
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

/// Build a [`FluffConfig`] from a poly [`EngineConfig`], for one side of the
/// lint/format partition.
///
/// Constructs an INI-format config string from user options and passes it to
/// [`FluffConfig::from_source`], which merges the user overrides on top of
/// sqruff's own embedded `default_config.cfg`.
///
/// Layering: sqruff defaults → opinionated poly override (`max_line_length`
/// 120) → user `poly.toml` options → the partition's suppressed groups.
///
/// The partition is applied through sqruff's own `exclude_rules` denylist, which
/// accepts group names as well as codes and is subtracted from the allowlist
/// last ([`RuleSet::get_rulepack`]). Suppressing at config level rather than
/// filtering violations afterwards means the suppressed rules never run — the
/// per-file path is hot. It also makes the partition unconditional: a user
/// `select = ["LT01"]` cannot pull a format-owned rule back into `lint`, mirroring
/// rumdl, whose format-owned suppression likewise ignores explicit enables.
///
/// [`RuleSet::get_rulepack`]: sqruff_lib::core::rules::RuleSet
fn build_fluff_config(cfg: &EngineConfig, mode: Mode) -> anyhow::Result<FluffConfig> {
    let dialect_str = cfg.options.get("dialect").and_then(|v| v.as_str()).unwrap_or("ansi");

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

    let mut ini = format!("[sqruff]\nmax_line_length = {}\n", cfg.globals.line_length);

    if dialect_str != "ansi" {
        ini.push_str(&format!("dialect = {dialect_str}\n"));
    }

    let selection = RuleSelection::from_options(cfg);

    let allow = warn_and_skip_blank(union_codes(string_list(cfg, "rules"), selection.select), "sqruff");
    if !allow.is_empty() {
        ini.push_str(&format!("rules = {}\n", allow.join(",")));
    }

    let mut deny = warn_and_skip_blank(
        union_codes(string_list(cfg, "exclude_rules"), selection.ignore),
        "sqruff",
    );
    deny.extend(mode.suppressed_groups().iter().map(|group| (*group).to_owned()));
    if !deny.is_empty() {
        ini.push_str(&format!("exclude_rules = {}\n", deny.join(",")));
    }

    // These become `[sqruff:rules:<name>]` INI sections that sqruff merges on
    if let Some(rule_configs) = cfg.options.get("rule_configs").and_then(|v| v.as_table()) {
        for (rule_name, rule_opts) in rule_configs {
            if let Some(opts_table) = rule_opts.as_table() {
                ini.push_str(&format!("\n[sqruff:rules:{rule_name}]\n"));
                for (key, val) in opts_table {
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

/// Convert a sqruff [`SQLBaseError`] to a poly [`Diagnostic`].
///
/// Parse and lex errors carry the sentinel code `"????"` (sqruff's internal
/// "no rule attached" marker).  These are structural failures — the file could
/// not be parsed — and are mapped to [`Severity::Error`].  Real rule violations
/// (any other code) are [`Severity::Warning`].
///
/// No diagnostic ever carries a `fix`: sqruff's autofixes are only reachable
/// through `lint_string(.., fix = true)`, which rewrites the whole file rather
/// than yielding per-violation edits, so they cannot be mapped onto [`Edit`]s
/// (hence `capabilities().fix == false`). `poly lint --fix` therefore has no way
/// to apply a sqruff repair, and in particular cannot apply a meaning-changing
/// one behind `format`'s back.
///
/// [`Edit`]: crate::engine::Edit
fn violation_to_diagnostic(violation: SQLBaseError) -> Diagnostic {
    let code_str = violation.rule_code();
    let is_parse_error = code_str == "????";
    Diagnostic {
        engine: "sqruff".to_string(),
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
