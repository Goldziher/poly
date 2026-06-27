//! Python backend: full rule-based linting via `ruff_linter` and formatting
//! via `ruff_python_formatter`.
//!
//! Both depend on the astral-sh/ruff git monorepo, pinned to rev
//! `03f787e51e94999977b9a5a32b0153d82d7e2142`. The `RUFF_REV` constant is
//! folded into [`RuffEngine::version`] so that upgrading the pin automatically
//! invalidates the polylint cache.
//!
//! # Opinionated rule selection
//!
//! The default selection extends ruff's built-in defaults (F + E4/E7/E9) with:
//!
//! | Code | Linter | Rationale |
//! |------|--------|-----------|
//! | `F`  | Pyflakes | undefined names, unused imports, etc. |
//! | `E4` | pycodestyle | import errors (E401/E402) |
//! | `E7` | pycodestyle | statement errors (E711…E743) |
//! | `E9` | pycodestyle | runtime/syntax errors (E999) |
//! | `W6` | pycodestyle | W605 — invalid escape sequence |
//! | `I`  | isort | import sorting |
//! | `UP` | pyupgrade | modernize Python syntax |
//! | `B`  | flake8-bugbear | common bugs and design issues |
//!
//! `E1`/`E2`/`E3`/`W1`/`W2`/`W3` are intentionally excluded — they overlap
//! with the ruff formatter and would fire on every well-formatted file.
//!
//! # Opinionated format defaults
//!
//! | Setting | Polylint default | ruff default |
//! |---------|-----------------|--------------|
//! | `line-length` | 120 | 88 |
//! | `docstring-code-format` | `true` | `false` |
//! | `docstring-code-line-width` | 120 | dynamic |
//!
//! These defaults are overridden by any `[fmt.python.ruff]` or
//! `[lint.python.ruff]` table in the user's `polylint.toml`.

use std::path::Path;
use std::str::FromStr;
use std::sync::OnceLock;

use ruff_db::diagnostic::Severity as RuffSeverity;
use ruff_formatter::LineWidth;
use ruff_linter::linter::{ParseSource, lint_only};
use ruff_linter::rule_selector::{PreviewOptions, RuleSelector};
use ruff_linter::settings::LinterSettings;
use ruff_linter::settings::flags;
use ruff_linter::settings::rule_table::RuleTable;
use ruff_linter::source_kind::SourceKind;
use ruff_python_ast::PySourceType;
use ruff_python_formatter::{DocstringCode, DocstringCodeLineWidth, PyFormatOptions};
use ruff_text_size::Ranged;

use crate::config::EngineConfig;
use crate::engine::{
    Capabilities, Diagnostic, Edit, Engine, FormatOutput, Severity, SourceFile, Span,
};
use crate::language::Language;

/// Opinionated rule selection: string codes resolved by [`RuleSelector::from_str`].
///
/// Extends ruff's built-in defaults (F + E4/E7/E9) with W6 (invalid escape),
/// I (isort), UP (pyupgrade), and B (flake8-bugbear). Omits E1/E2/E3/W1/W2/W3
/// because the ruff formatter already handles those whitespace/blank-line rules.
static RULE_CODES: &[&str] = &["F", "E4", "E7", "E9", "W6", "I", "UP", "B"];

/// Resolve a list of rule-code strings to ruff `Rule`s.
fn rules_for_codes(codes: &[String]) -> Vec<ruff_linter::registry::Rule> {
    let preview = PreviewOptions::default();
    // Collect to a `Vec` first because `RuleSelector::rules` returns an iterator
    // that borrows the selector — it cannot escape the closure.
    codes
        .iter()
        .filter_map(|s| RuleSelector::from_str(s).ok())
        .flat_map(|sel| sel.rules(&preview).collect::<Vec<_>>())
        .collect()
}

/// Build a [`RuleTable`] from a selected set of codes minus an ignored set.
fn build_rule_table(select: &[String], ignore: &[String]) -> RuleTable {
    let selected = rules_for_codes(select);
    let ignored = rules_for_codes(ignore);
    RuleTable::from_iter(selected.into_iter().filter(|rule| !ignored.contains(rule)))
}

/// Read an array-of-strings option from the engine config.
fn string_list(cfg: &EngineConfig, key: &str) -> Vec<String> {
    cfg.options
        .get(key)
        .and_then(toml::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// The opinionated default [`LinterSettings`], built once and shared.
///
/// `LinterSettings` is `Clone + Send + Sync`; the `OnceLock` ensures it is
/// built at most once and then borrowed concurrently from the rayon thread
/// pool. Used as the fast path when no `[lint.python.ruff]` config is present.
fn default_settings() -> &'static LinterSettings {
    static SETTINGS: OnceLock<LinterSettings> = OnceLock::new();
    SETTINGS.get_or_init(|| {
        let codes: Vec<String> = RULE_CODES.iter().map(|s| (*s).to_owned()).collect();
        let mut settings = LinterSettings::new(Path::new("."));
        settings.rules = build_rule_table(&codes, &[]);
        settings.line_length = ruff_linter::line_width::LineLength::try_from(120_u16)
            .expect("120 is a valid line length");
        settings
    })
}

/// Build [`LinterSettings`] from user config, layered over the opinionated base.
///
/// Honors `[lint.python.ruff]` keys: `select` (replaces the default rule set),
/// `extend_select` (adds to it), `ignore` (removes rules), and `line_length`
/// (overriding the global default — only affects line-length rules, which the
/// default set omits). Called only when config options are present; the empty
/// case uses [`default_settings`] to avoid rebuilding per file.
fn build_settings(cfg: &EngineConfig) -> LinterSettings {
    let select = string_list(cfg, "select");
    let extend_select = string_list(cfg, "extend_select");
    let ignore = string_list(cfg, "ignore");

    let mut codes: Vec<String> = if select.is_empty() {
        RULE_CODES.iter().map(|s| (*s).to_owned()).collect()
    } else {
        select
    };
    codes.extend(extend_select);

    let line_length = cfg
        .options
        .get("line_length")
        .and_then(toml::Value::as_integer)
        .map(|v| v as usize)
        .unwrap_or(cfg.globals.line_length);

    let mut settings = LinterSettings::new(Path::new("."));
    settings.rules = build_rule_table(&codes, &ignore);
    settings.line_length = u16::try_from(line_length)
        .ok()
        .and_then(|w| ruff_linter::line_width::LineLength::try_from(w).ok())
        .unwrap_or_else(|| {
            ruff_linter::line_width::LineLength::try_from(120_u16).expect("120 is valid")
        });
    settings
}

/// Convert a ruff [`RuffSeverity`] to the polylint [`Severity`].
fn map_severity(s: RuffSeverity) -> Severity {
    match s {
        RuffSeverity::Info => Severity::Info,
        RuffSeverity::Warning => Severity::Warning,
        RuffSeverity::Error | RuffSeverity::Fatal => Severity::Error,
    }
}

/// Ruff Python backend (lint + format).
pub struct RuffEngine;

static LANGUAGES: &[Language] = &[Language::Python];

impl Engine for RuffEngine {
    fn name(&self) -> &'static str {
        "ruff"
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

    /// Version string incorporates the pinned ruff git rev so that upgrading
    /// the rev automatically invalidates any cached lint/format output.
    fn version(&self) -> &str {
        concat!("git-ruff:", "03f787e51e94999977b9a5a32b0153d82d7e2142")
    }

    fn lint(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        // Fast path: no `[lint.python.ruff]` config → reuse the shared default
        // settings. Only build per-call settings when the user configured rules.
        let owned_settings;
        let settings = if cfg.options.is_empty() {
            default_settings()
        } else {
            owned_settings = build_settings(cfg);
            &owned_settings
        };

        let is_stub = src.path.extension().is_some_and(|e| e == "pyi");
        let source_kind = SourceKind::Python {
            code: src.content.to_string(),
            is_stub,
        };
        let source_type = if is_stub {
            PySourceType::Stub
        } else {
            PySourceType::Python
        };

        let result = lint_only(
            &src.path,
            None, // no package-root; isort treats all imports as third-party
            settings,
            flags::Noqa::Enabled,
            &source_kind,
            source_type,
            ParseSource::None,
        );

        let diagnostics = result
            .diagnostics
            .into_iter()
            .map(|ruff_diag| {
                let code = ruff_diag.secondary_code().map(|c| c.as_str().to_string());
                let severity = map_severity(ruff_diag.severity());
                let message = ruff_diag.primary_message().to_string();

                let span = ruff_diag
                    .ruff_start_location()
                    .zip(ruff_diag.ruff_end_location())
                    .map(|(start, end)| Span {
                        start_line: start.line.get() as u32,
                        start_col: start.column.get() as u32,
                        end_line: end.line.get() as u32,
                        end_col: end.column.get() as u32,
                    });

                // Only auto-apply fixes that ruff marks `Safe` and that consist
                // of exactly one edit. Our `Diagnostic` carries a single `Edit`,
                // so a multi-edit fix cannot be applied atomically — applying a
                // subset would corrupt the file. `Unsafe`/`DisplayOnly` fixes and
                // multi-edit fixes still surface as diagnostics, just without an
                // autofix. (Multi-edit fix support is tracked as a follow-up.)
                let fix = ruff_diag
                    .fix()
                    .filter(|f| f.applicability().is_safe() && f.edits().len() == 1)
                    .and_then(|f| {
                        f.edits().first().map(|edit| Edit {
                            start_byte: edit.start().to_usize(),
                            end_byte: edit.end().to_usize(),
                            replacement: edit.content().unwrap_or("").to_string(),
                        })
                    });

                Diagnostic {
                    engine: "ruff".to_string(),
                    code,
                    severity,
                    message,
                    span,
                    fix,
                    metadata: Default::default(),
                }
            })
            .collect();

        Ok(diagnostics)
    }

    fn format(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
        let line_width = u16::try_from(cfg.globals.line_length)
            .ok()
            .and_then(|w| LineWidth::try_from(w).ok())
            .unwrap_or_else(|| LineWidth::try_from(120_u16).unwrap());

        let options = PyFormatOptions::from_extension(&src.path)
            .with_line_width(line_width)
            .with_docstring_code(DocstringCode::Enabled)
            .with_docstring_code_line_width(DocstringCodeLineWidth::Fixed(line_width));

        match ruff_python_formatter::format_module_source(&src.content, options) {
            Ok(printed) => {
                let formatted = printed.into_code();
                if formatted == *src.content {
                    Ok(FormatOutput::Unchanged)
                } else {
                    Ok(FormatOutput::Formatted(formatted))
                }
            }
            Err(err) => Err(anyhow::anyhow!("ruff format error: {err}")),
        }
    }
}
