//! R backend: formatting via [`air_r_formatter`], linting via [`jarl_core`].
//!
//! `air` is a pure-Rust R formatter backed by a Posit-forked biome CST engine.
//! `jarl` is a pure-Rust R linter that reuses the same air parser.  Both run
//! in-process — no subprocess, no system dependency.
//!
//! ## Capabilities
//! - **Format**: reformat `.R` files with opinionated overrides (line width 120,
//!   indent width from [`EngineConfig`], space indent).
//! - **Lint**: run jarl's default-enabled rule set against `.R` files.
//! - **Fix**: apply safe jarl autofixes (byte-range edits) produced by the lint
//!   rules that advertise a [`jarl_core::rule_set::FixStatus::Safe`] fix.
//!
//! ## Config layering
//!
//! ### `[lint.r.r]`
//!
//! Uses the shared `RuleSelection` schema:
//!
//! | Key | Type | Purpose |
//! |-----|------|---------|
//! | `select` | `[String]` | Replace the default rule set. Pass rule codes or jarl category group names (`COMM`, `CORR`, `SUSP`, `PERF`, `READ`, `TESTTHAT`, `DPLYR`) or the special value `ALL`. Names are case-sensitive. |
//! | `extend_select` | `[String]` | Add to the default rule set. |
//! | `ignore` | `[String]` | Remove from the active set. |
//! | `[rules.<id>]` | table | Per-rule overrides. `level` can be `"error"`, `"warning"`, `"info"`, `"hint"`. |
//!
//! When `select`/`extend_select`/`ignore` are all absent the global
//! [`JARL_CONFIG`] static is reused (fast path — no per-file config rebuild).
//!
//! ### `[fmt.r.r]`
//!
//! | Key | Type | Values | Default |
//! |-----|------|--------|---------|
//! | `indent_style` | string | `"space"`, `"tab"` | `"space"` |
//! | `indent_width` | integer | 1–24 | from `EngineConfig.indent_width` |
//! | `line_ending` | string | `"lf"`, `"crlf"` | `"lf"` |
//! | `persistent_line_breaks` | string | `"respect"`, `"ignore"` | `"respect"` |
//! | `assignment_style` | string | `"arrow"`, `"equal"`, `"preserve"` | `"arrow"` |
//!
//! Line length is controlled by the global `line_length` key, not duplicated
//! here.
//!
//! ## Cache key
//! [`VERSION`] folds both the air and jarl git revs so any fork bump invalidates
//! stale cached output.  The runner also folds `cfg.options` (the resolved
//! `[lint.r.r]` / `[fmt.r.r]` table) into the cache key via
//! `ResultCache::serialize_args`, so a static `VERSION` is sufficient — config
//! changes naturally invalidate cache entries without a version bump.

use std::collections::HashMap;
use std::sync::LazyLock;

use air_r_formatter::context::RFormatOptions;
use air_r_formatter::format_node;
use air_r_parser::RParserOptions;
use air_settings::{AssignmentStyle, IndentStyle, IndentWidth, LineEnding, LineWidth, PersistentLineBreaks};
use jarl_core::check::get_checks as jarl_get_checks;
use jarl_core::config::{ArgsConfig, Config, build_config};
use jarl_core::diagnostic::Diagnostic as JarlDiagnostic;
use jarl_core::package::{FilePackageInfo, PackageAnalysis, PackageContext};

use crate::config::EngineConfig;
use crate::engine::{Capabilities, Diagnostic, Edit, Engine, FormatOutput, Severity, SourceFile, Span};
use crate::engines::rule_config::RuleSelection;
use crate::language::Language;

/// Cache key version: folds both the air and jarl git revs so any fork bump
/// invalidates stale cached output.
///
/// - air rev: `c916545f14f76e1d6bd6ff918870f86dfa704b63` (first 7 chars)
/// - jarl rev: `61c0ce75deae7402cdf60ec947e089a4ae484e79` (first 7 chars)
const VERSION: &str = "air:c916545 jarl:61c0ce7";

/// Tier-1 languages handled by this backend.
static LANGUAGES: &[Language] = &[Language::R];

/// jarl [`Config`] built once at first lint call and reused across the rayon
/// `par_iter`.  Building config is cheap (no filesystem access with empty paths),
/// but the resulting `Config` is `Send + Sync` so it can safely live in a
/// `LazyLock` and be shared across threads without additional locking.
///
/// Rule selection: default-enabled rules, no minimum R version specified
/// (conservative — version-gated rules such as `grepv` are excluded).
static JARL_CONFIG: LazyLock<Config> = LazyLock::new(|| {
    let args = ArgsConfig {
        files: vec![],
        fix: false,
        unsafe_fixes: false,
        fix_only: false,
        // Empty select → `all_rules_enabled_by_default()` (excludes opt-in
        // categories like Dplyr that need a live R package cache to be useful).
        select: String::new(),
        extend_select: String::new(),
        ignore: String::new(),
        // No minimum version → version-gated rules are excluded (conservative).
        min_r_version: None,
        // polylint is not a VCS-aware fix tool; these flags only affect the fix
        // path which we never trigger from this engine.
        allow_dirty: true,
        allow_no_vcs: true,
        assignment: None,
    };
    // Empty paths → `determine_minimum_r_version` loops over nothing (no FS
    // access).  This is safe to call at init time.
    build_config(&args, None, vec![]).expect("jarl: failed to build default config — this is a bug in polylint")
});

/// Tier-1 R backend — formats and lints `.R` files using the `air` formatter
/// and `jarl` linter in-process.
pub struct REngine;

impl Engine for REngine {
    fn name(&self) -> &'static str {
        "r"
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
        VERSION
    }

    /// Lint `src.content` with jarl.
    ///
    /// Uses an in-process call to [`jarl_core::check::get_checks`] with empty
    /// cross-file package context — polylint is a per-file linter and does not
    /// perform multi-file package analysis.  Package-specific rules (unused
    /// function, duplicated definition) still run but produce no diagnostics
    /// without cross-file data, which is correct: false negatives are preferable
    /// to false positives in a general-purpose linter.
    ///
    /// Parse errors are silently swallowed (`Ok(vec![])`) so that a broken R
    /// file does not surface confusing "parse error" diagnostics.  The formatter
    /// already handles parse errors by returning `Unchanged`.
    ///
    /// Rule selection from `[lint.r.r]` is applied by building a per-call
    /// [`Config`] from a `[lint.r.r]` `select`/`extend_select`/`ignore`.
    /// When no selection config is present the global [`JARL_CONFIG`] static is
    /// reused (fast path — avoids rebuilding an identical config per file).
    fn lint(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        let selection = RuleSelection::from_options(cfg);

        // Build jarl Config: reuse the global default when no selection is
        // provided so the common case pays zero per-file allocation.
        let owned_config;
        let jarl_cfg: &Config = if selection.is_empty() {
            &JARL_CONFIG
        } else {
            let args = ArgsConfig {
                files: vec![],
                fix: false,
                unsafe_fixes: false,
                fix_only: false,
                select: selection.select.join(","),
                extend_select: selection.extend_select.join(","),
                ignore: selection.ignore.join(","),
                min_r_version: None,
                allow_dirty: true,
                allow_no_vcs: true,
                assignment: None,
            };
            // Propagate UnknownRulesError so the user sees a clear message
            // about invalid rule codes/category names in [lint.r.r].
            owned_config = build_config(&args, None, vec![])?;
            &owned_config
        };

        let pkg = PackageAnalysis::default();
        let pkg_contexts: HashMap<_, PackageContext> = HashMap::new();
        let file_pkg_info: HashMap<_, FilePackageInfo> = HashMap::new();

        match jarl_get_checks(&src.content, &src.path, jarl_cfg, &pkg, &pkg_contexts, &file_pkg_info) {
            Ok(jarl_diags) => Ok(jarl_diags
                .into_iter()
                .map(|d| {
                    let mut diag = map_jarl_diagnostic(d, &src.content);
                    // Apply per-rule severity override from
                    // [lint.r.r.rules.<id>] level = "error"|"warning"|...
                    if let Some(code) = diag.code.as_deref()
                        && let Some(opts) = selection.rules.get(code)
                        && let Some(level) = opts.level
                    {
                        diag.severity = level;
                    }
                    diag
                })
                .collect()),
            Err(e) if e.is::<jarl_core::error::ParseError>() => {
                // Corrupt/partial R — graceful degradation; the format path
                // returns Unchanged for the same input.
                Ok(vec![])
            }
            Err(e) => Err(e),
        }
    }

    /// Format `src.content` with air. Returns [`FormatOutput::Unchanged`] when:
    /// - the formatter output equals the input (file is already well-formatted), or
    /// - the file has parse errors (corrupt/partial R is left untouched).
    ///
    /// All `[fmt.r.r]` keys are optional; unrecognised string values are silently
    /// ignored and the option falls back to its polylint default.
    fn format(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
        let parse = air_r_parser::parse(&src.content, RParserOptions::default());

        // Parse error → leave the file untouched rather than risk data loss.
        if parse.has_error() {
            return Ok(FormatOutput::Unchanged);
        }

        // Build options from the resolved EngineConfig.
        // line_length is usize; LineWidth accepts u16 in the range 1..=320.
        // If the value is out of range, fall back to air's default (80) which is
        // overridden to 120 by the GlobalDefaults.
        let line_width = u16::try_from(cfg.globals.line_length)
            .ok()
            .and_then(|w| LineWidth::try_from(w).ok())
            .unwrap_or_default();
        // indent_width is usize; IndentWidth accepts values 1..=24.
        let indent_width = IndentWidth::try_from(cfg.indent_width).unwrap_or_default();

        // Parse optional [fmt.r.r] keys; silently fall back to polylint defaults
        // for absent or unrecognised values (no user-visible error — unknown
        // option strings are a minor misconfiguration, not a crash).
        let indent_style = cfg
            .options
            .get("indent_style")
            .and_then(toml::Value::as_str)
            .and_then(|s| s.parse::<IndentStyle>().ok())
            .unwrap_or(IndentStyle::Space); // opinionated override: always Space

        let line_ending = cfg
            .options
            .get("line_ending")
            .and_then(toml::Value::as_str)
            .and_then(|s| match s {
                "lf" => Some(LineEnding::Lf),
                "crlf" => Some(LineEnding::Crlf),
                _ => None,
            })
            // Default to poly's global line-ending (so `[defaults] line_ending`
            // reaches R formatting like it does YAML), not air's own default.
            .unwrap_or(match cfg.globals.line_ending {
                crate::config::LineEnding::Crlf => LineEnding::Crlf,
                crate::config::LineEnding::Lf => LineEnding::Lf,
            });

        let persistent_line_breaks = cfg
            .options
            .get("persistent_line_breaks")
            .and_then(toml::Value::as_str)
            .and_then(|s| s.parse::<PersistentLineBreaks>().ok())
            .unwrap_or_default(); // air default: Respect

        let assignment_style = cfg
            .options
            .get("assignment_style")
            .and_then(toml::Value::as_str)
            .and_then(|s| s.parse::<AssignmentStyle>().ok())
            .unwrap_or_default(); // air default: Arrow (<-)

        let opts = RFormatOptions::new()
            .with_line_width(line_width)
            .with_indent_style(indent_style)
            .with_indent_width(indent_width)
            .with_line_ending(line_ending)
            .with_persistent_line_breaks(persistent_line_breaks)
            .with_assignment_style(assignment_style);

        let code = format_node(opts, &parse.syntax())
            .map_err(|e| anyhow::anyhow!("air: format_node failed: {e}"))?
            .print()
            .map_err(|e| anyhow::anyhow!("air: print failed: {e}"))?
            .into_code();

        if code == src.content.as_ref() {
            Ok(FormatOutput::Unchanged)
        } else {
            Ok(FormatOutput::Formatted(code))
        }
    }
}

// ---------------------------------------------------------------------------
// Diagnostic mapping helpers
// ---------------------------------------------------------------------------

/// Convert a byte offset into a 1-based (line, column) pair using the source
/// content.  Clamps the offset to the content length so out-of-bounds offsets
/// do not panic.
fn byte_to_span_pos(content: &str, byte_offset: usize) -> (u32, u32) {
    let safe = byte_offset.min(content.len());
    let before = &content[..safe];
    let line = before.bytes().filter(|&b| b == b'\n').count() as u32 + 1;
    let col_start = before.rfind('\n').map_or(0, |p| p + 1);
    // Column is the number of bytes from the last newline (or SOF) to the offset,
    // plus 1 for 1-based indexing.
    let col = (safe - col_start) as u32 + 1;
    (line, col)
}

/// Map a [`JarlDiagnostic`] to a polylint [`Diagnostic`].
///
/// Severity is always [`Severity::Warning`] — jarl violations have no severity
/// field; they are all style/correctness warnings, never fatal errors.
///
/// A fix edit is included only when [`JarlDiagnostic::has_safe_fix`] returns
/// `true`: the jarl fix is not marked `to_skip`, has non-empty replacement
/// content, **and** the rule's [`FixStatus`] is `Safe`.  Rules with `Unsafe`
/// fix status (e.g. `all_equal`, `condition_call`, `nzchar`, `pipe_consistency`)
/// are not applied automatically — they could change program semantics and
/// require human review.
fn map_jarl_diagnostic(jarl_diag: JarlDiagnostic, content: &str) -> Diagnostic {
    let start_byte: usize = jarl_diag.range.start().into();
    let end_byte: usize = jarl_diag.range.end().into();
    let (start_line, start_col) = byte_to_span_pos(content, start_byte);
    let (end_line, end_col) = byte_to_span_pos(content, end_byte);

    let fix = if jarl_diag.has_safe_fix() {
        vec![Edit {
            start_byte: jarl_diag.fix.start,
            end_byte: jarl_diag.fix.end,
            replacement: jarl_diag.fix.content,
        }]
    } else {
        vec![]
    };

    Diagnostic {
        engine: "r".to_string(),
        code: Some(jarl_diag.message.name),
        severity: Severity::Warning,
        title: jarl_diag.message.body,
        description: None,
        url: None,
        span: Some(Span {
            start_line,
            start_col,
            end_line,
            end_col,
        }),
        fix,
        metadata: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::config::GlobalDefaults;

    fn make_src(content: &str) -> SourceFile {
        SourceFile {
            path: PathBuf::from("test.R"),
            language: Language::R,
            content: content.into(),
        }
    }

    fn default_cfg() -> EngineConfig {
        EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 2,
            options: toml::Table::new(),
        }
    }

    /// Build an [`EngineConfig`] from a TOML snippet that represents the
    /// options table (e.g. `select = [\"CORR\"]`).
    fn cfg_from_toml(toml_str: &str) -> EngineConfig {
        EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 2,
            options: toml::from_str(toml_str).expect("valid TOML in test"),
        }
    }

    #[test]
    fn engine_metadata() {
        let engine = REngine;
        assert_eq!(engine.name(), "r");
        assert_eq!(engine.languages(), &[Language::R]);
        let caps = engine.capabilities();
        assert!(caps.lint, "lint capability must be true");
        assert!(caps.format, "format capability must be true");
        assert!(caps.fix, "fix capability must be true");
        assert_eq!(engine.version(), VERSION);
    }

    #[test]
    fn lint_clean_r_has_no_diagnostics() {
        // Clean, idiomatic R — should trigger zero jarl rules.
        let engine = REngine;
        let src = make_src("x <- 1\n");
        let diags = engine.lint(&src, &default_cfg()).unwrap();
        assert!(diags.is_empty(), "expected no diagnostics for clean R, got: {diags:?}");
    }

    #[test]
    fn lint_parse_error_returns_empty() {
        // A bare `function(` never closes — jarl returns ParseError.
        // polylint must swallow it and return Ok(vec![]).
        let engine = REngine;
        let src = make_src("function(\n");
        let diags = engine.lint(&src, &default_cfg()).unwrap();
        assert!(diags.is_empty(), "parse error must degrade to empty diagnostics");
    }

    #[test]
    fn lint_equals_na_produces_diagnostic_with_fix() {
        // `x == NA` is flagged by the `equals_na` rule (default-enabled, safe fix).
        let engine = REngine;
        let src = make_src("x <- c(1, 2, NA)\ny <- x == NA\n");
        let diags = engine.lint(&src, &default_cfg()).unwrap();

        let equals_na: Vec<_> = diags
            .iter()
            .filter(|d| d.code.as_deref() == Some("equals_na"))
            .collect();
        assert!(
            !equals_na.is_empty(),
            "expected at least one equals_na diagnostic, got: {diags:?}"
        );

        let d = &equals_na[0];
        assert_eq!(d.engine, "r");
        assert_eq!(d.severity, Severity::Warning);
        // The fix replaces `x == NA` with `is.na(x)`.
        assert!(!d.fix.is_empty(), "equals_na must include an autofix Edit");
        assert!(d.span.is_some(), "equals_na must include a source Span");

        let span = d.span.unwrap();
        // The diagnostic is on the second line (y <- x == NA), so start_line >= 2.
        assert!(
            span.start_line >= 2,
            "equals_na span must point to the second line, got line {}",
            span.start_line
        );
    }

    #[test]
    fn unformatted_input_returns_formatted() {
        let engine = REngine;
        let src = make_src("x<-1+2\nf<-function(a,b){a+b}\n");
        let out = engine.format(&src, &default_cfg()).unwrap();
        assert!(
            matches!(out, FormatOutput::Formatted(_)),
            "expected Formatted for unformatted input"
        );
    }

    #[test]
    fn already_formatted_input_is_unchanged() {
        let engine = REngine;
        // This is the canonical air-formatted output for the unformatted fixture.
        let formatted = "x <- 1 + 2\nf <- function(a, b) {\n  a + b\n}\n";
        let src = make_src(formatted);
        let out = engine.format(&src, &default_cfg()).unwrap();
        assert!(
            matches!(out, FormatOutput::Unchanged),
            "expected Unchanged for already-formatted input"
        );
    }

    #[test]
    fn unparsable_input_is_unchanged() {
        let engine = REngine;
        // A bare `function(` never closes; air should report a parse error.
        let src = make_src("function(\n");
        let out = engine.format(&src, &default_cfg()).unwrap();
        assert!(
            matches!(out, FormatOutput::Unchanged),
            "expected Unchanged for unparsable input"
        );
    }

    // ── Config-wiring tests (Phase 2) ───────────────────────────────────────

    #[test]
    fn lint_default_fires_perf_rule() {
        // `any(duplicated(x))` triggers the `any_duplicated` PERF rule by default.
        let engine = REngine;
        let src = make_src("any(duplicated(c(1, 2, 1)))\n");
        let diags = engine.lint(&src, &default_cfg()).unwrap();
        let codes: Vec<_> = diags.iter().filter_map(|d| d.code.as_deref()).collect();
        assert!(
            codes.contains(&"any_duplicated"),
            "expected any_duplicated in default rule set, got: {codes:?}"
        );
    }

    #[test]
    fn lint_select_corr_filters_out_perf_rule() {
        // With select = ["CORR"], only Correctness rules are active.
        // `any_duplicated` is a PERF rule and must NOT appear.
        let engine = REngine;
        let src = make_src("any(duplicated(c(1, 2, 1)))\n");
        let cfg = cfg_from_toml(r#"select = ["CORR"]"#);
        let diags = engine.lint(&src, &cfg).unwrap();
        let codes: Vec<_> = diags.iter().filter_map(|d| d.code.as_deref()).collect();
        assert!(
            !codes.contains(&"any_duplicated"),
            "any_duplicated must be absent when select=[\"CORR\"], got: {codes:?}"
        );
    }

    #[test]
    fn lint_ignore_equals_na_drops_it() {
        // `x == NA` triggers `equals_na` by default.
        // With ignore = ["equals_na"], that diagnostic must be absent.
        let engine = REngine;
        let src = make_src("x <- c(1, 2, NA)\ny <- x == NA\n");

        // Sanity-check: the rule fires without ignore.
        let default_diags = engine.lint(&src, &default_cfg()).unwrap();
        let has_without_ignore = default_diags.iter().any(|d| d.code.as_deref() == Some("equals_na"));
        assert!(has_without_ignore, "equals_na must fire by default");

        // Now with ignore: it must disappear.
        let cfg = cfg_from_toml(r#"ignore = ["equals_na"]"#);
        let diags = engine.lint(&src, &cfg).unwrap();
        let has_with_ignore = diags.iter().any(|d| d.code.as_deref() == Some("equals_na"));
        assert!(!has_with_ignore, "equals_na must be absent when in ignore list");
    }

    #[test]
    fn lint_rule_level_override_to_error_changes_severity() {
        // `equals_na` normally maps to Severity::Warning.
        // A [rules.equals_na] level = "error" override must produce Severity::Error.
        let engine = REngine;
        let src = make_src("x <- c(1, 2, NA)\ny <- x == NA\n");
        let cfg = cfg_from_toml(
            r#"
[rules.equals_na]
level = "error"
"#,
        );
        let diags = engine.lint(&src, &cfg).unwrap();
        let equals_na: Vec<_> = diags
            .iter()
            .filter(|d| d.code.as_deref() == Some("equals_na"))
            .collect();
        assert!(!equals_na.is_empty(), "equals_na must still fire with level override");
        assert_eq!(
            equals_na[0].severity,
            Severity::Error,
            "severity must be overridden to Error"
        );
    }

    #[test]
    fn lint_unsafe_fix_rule_has_no_edit() {
        // `all_equal` has FixStatus::Unsafe in jarl's rule_set.
        // polylint must NOT emit an Edit for it; doing so would silently apply a
        // semantics-changing transformation under `poly lint --fix`.
        //
        // Trigger: `isFALSE(all.equal(x, y))` is the canonical pattern the rule
        // detects.  The jarl fix content is non-empty (rewrites to
        // `!isTRUE(all.equal(x, y))`), so the old `!to_skip && !content.is_empty()`
        // guard incorrectly emitted an Edit.  The new `has_safe_fix()` guard rejects it.
        let engine = REngine;
        let src = make_src("if (isFALSE(all.equal(x, y))) stop(\"different\")\n");
        let diags = engine.lint(&src, &default_cfg()).unwrap();
        let all_equal_diags: Vec<_> = diags
            .iter()
            .filter(|d| d.code.as_deref() == Some("all_equal"))
            .collect();
        assert!(
            !all_equal_diags.is_empty(),
            "expected at least one all_equal diagnostic; got: {diags:?}"
        );
        assert!(
            all_equal_diags.iter().all(|d| d.fix.is_empty()),
            "all_equal has Unsafe fix status; Edit must not be emitted under \
             --fix; got: {all_equal_diags:#?}"
        );
    }

    #[test]
    fn lint_unknown_rule_select_returns_err() {
        // When `select` contains a completely unknown rule code, `build_config`
        // propagates `jarl_core::error::UnknownRulesError` as `Err`.
        // This exercises the error path that was previously untested.
        let engine = REngine;
        let src = make_src("x <- 1\n");
        let cfg = cfg_from_toml(r#"select = ["TOTALLY_FAKE_RULE"]"#);
        let result = engine.lint(&src, &cfg);
        assert!(result.is_err(), "expected Err for unknown rule in select, got Ok");
    }

    #[test]
    fn fmt_assignment_style_equal_changes_arrow_to_equals() {
        // `assignment_style = "equal"` rewrites top-level `<-` to `=`.
        let engine = REngine;
        let src = make_src("x <- 1\n");

        // Default (Arrow): already formatted — must be Unchanged.
        let default_out = engine.format(&src, &default_cfg()).unwrap();
        assert!(
            matches!(default_out, FormatOutput::Unchanged),
            "x <- 1 must be Unchanged under default Arrow style"
        );

        // Equal style: `<-` → `=`.
        let cfg = cfg_from_toml(r#"assignment_style = "equal""#);
        let equal_out = engine.format(&src, &cfg).unwrap();
        match equal_out {
            FormatOutput::Formatted(code) => {
                assert!(
                    code.contains("x = 1"),
                    "assignment_style=equal must rewrite <- to =, got: {code:?}"
                );
            }
            FormatOutput::Unchanged => {
                panic!("expected Formatted with assignment_style=equal, got Unchanged");
            }
        }
    }
}
