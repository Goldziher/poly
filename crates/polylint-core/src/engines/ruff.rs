//! Python backend: full rule-based linting via `ruff_linter` and formatting
//! via `ruff_python_formatter`.
//!
//! Both depend on the astral-sh/ruff git monorepo, pinned to rev
//! `1cb20127c47cf5c66ead93fb39e47600c857bb7e`. The `RUFF_REV` constant is
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
use ruff_linter::package::PackageRoot;
use ruff_linter::packaging::detect_package_root;
use ruff_linter::rule_selector::{PreviewOptions, RuleSelector};
use ruff_linter::rules::isort::categorize::KnownModules;
use ruff_linter::rules::pydocstyle::settings::Convention as PydocstyleConvention;
use ruff_linter::settings::LinterSettings;
use ruff_linter::settings::flags;
use ruff_linter::settings::rule_table::RuleTable;
use ruff_linter::settings::types::IdentifierPattern;
use ruff_linter::source_kind::SourceKind;
use ruff_python_ast::PySourceType;
use ruff_python_formatter::{DocstringCode, DocstringCodeLineWidth, PyFormatOptions};
use ruff_text_size::Ranged;
use rustc_hash::FxHashMap;

use crate::config::EngineConfig;
use crate::engine::{Capabilities, Diagnostic, Edit, Engine, FormatOutput, Severity, SourceFile, Span};
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
        .filter_map(|s| match RuleSelector::from_str(s) {
            Ok(selector) => Some(selector),
            Err(_) => {
                // Warn and skip rather than dropping silently (ADR 0016): a code
                // that ruff cannot resolve is a user error worth surfacing.
                tracing::warn!(code = %s, engine = "ruff", "unknown rule or category; skipping");
                None
            }
        })
        .flat_map(|sel| sel.rules(&preview).collect::<Vec<_>>())
        .collect()
}

/// Build a [`RuleTable`] from a selected set of codes minus an ignored set.
fn build_rule_table(select: &[String], ignore: &[String]) -> RuleTable {
    let selected = rules_for_codes(select);
    let ignored = rules_for_codes(ignore);
    RuleTable::from_iter(selected.into_iter().filter(|rule| !ignored.contains(rule)))
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
        settings.line_length =
            ruff_linter::line_width::LineLength::try_from(120_u16).expect("120 is a valid line length");
        // E501 (line-too-long) reads `pycodestyle.max_line_length`, which falls back to
        // ruff's hardcoded 88 when unset — not the global `line_length`. Mirror it so the
        // linter's line-length rule agrees with the formatter width.
        settings.pycodestyle.max_line_length = settings.line_length;
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
    // Parse the canonical `select` / `extend_select` / `ignore` vocabulary
    // through the shared parser (ADR 0016) rather than reading each key ad hoc.
    let selection = super::rule_config::RuleSelection::from_options(cfg);

    let mut codes: Vec<String> = if selection.select.is_empty() {
        RULE_CODES.iter().map(|s| (*s).to_owned()).collect()
    } else {
        selection.select
    };
    codes.extend(selection.extend_select);
    let ignore = selection.ignore;

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
        .unwrap_or_else(|| ruff_linter::line_width::LineLength::try_from(120_u16).expect("120 is valid"));
    // E501 (line-too-long) reads `pycodestyle.max_line_length`, which falls back to
    // ruff's hardcoded 88 when unset — not the global `line_length`. Mirror it so the
    // linter's line-length rule agrees with the configured width and the formatter.
    settings.pycodestyle.max_line_length = settings.line_length;

    // Per-plugin parameters (matching ruff's own option names, flattened).
    let usize_opt = |key: &str| {
        cfg.options
            .get(key)
            .and_then(toml::Value::as_integer)
            .and_then(|v| usize::try_from(v).ok())
    };
    if let Some(v) = usize_opt("mccabe_max_complexity") {
        settings.mccabe.max_complexity = v;
    }
    if let Some(v) = usize_opt("pylint_max_args") {
        settings.pylint.max_args = v;
    }
    if let Some(v) = usize_opt("pylint_max_branches") {
        settings.pylint.max_branches = v;
    }
    if let Some(v) = usize_opt("pylint_max_returns") {
        settings.pylint.max_returns = v;
    }

    // pydocstyle convention: set it AND disable the D-rules that convention
    // turns off (ruff applies this at config-resolution time; poly builds the
    // rule table by hand, so do it explicitly).
    if let Some(convention) = cfg
        .options
        .get("pydocstyle_convention")
        .and_then(toml::Value::as_str)
        .and_then(|s| match s {
            "google" => Some(PydocstyleConvention::Google),
            "numpy" => Some(PydocstyleConvention::Numpy),
            "pep257" => Some(PydocstyleConvention::Pep257),
            _ => None,
        })
    {
        settings.pydocstyle.convention = Some(convention);
        for rule in convention.rules_to_be_ignored() {
            settings.rules.disable(*rule);
        }
    }

    // target-version: gates version-specific rules (e.g. pyupgrade). Accept both
    // ruff's canonical `py310` spelling and the dotted `3.10` form.
    if let Some(version) = cfg
        .options
        .get("target_version")
        .and_then(toml::Value::as_str)
        .and_then(parse_python_version)
    {
        settings.unresolved_target_version = version.into();
    }

    // isort: known-first-party and known-third-party — classify modules that
    // the package-root walk cannot discover (e.g. src-layout first-party
    // packages tested from a sibling `tests/` directory).
    let str_list = |key: &str| -> Vec<String> {
        cfg.options
            .get(key)
            .and_then(toml::Value::as_array)
            .map(|arr| arr.iter().filter_map(toml::Value::as_str).map(str::to_owned).collect())
            .unwrap_or_default()
    };

    // src: first-party source roots for import classification (isort). Mirrors
    // ruff's `src`; only overrides the default when explicitly provided.
    let src_roots = str_list("src");
    if !src_roots.is_empty() {
        settings.src = src_roots.iter().map(std::path::PathBuf::from).collect();
    }
    let known_first_party: Vec<IdentifierPattern> = str_list("known_first_party")
        .iter()
        .filter_map(|s| IdentifierPattern::new(s).ok())
        .collect();
    let known_third_party: Vec<IdentifierPattern> = str_list("known_third_party")
        .iter()
        .filter_map(|s| IdentifierPattern::new(s).ok())
        .collect();
    if !known_first_party.is_empty() || !known_third_party.is_empty() {
        settings.isort.known_modules = KnownModules::new(
            known_first_party,
            known_third_party,
            vec![],
            vec![],
            FxHashMap::default(),
        );
    }

    settings
}

/// Parse a Python target version from either ruff's canonical `py310` spelling
/// or the dotted `3.10` form. Returns `None` for anything unrecognised so the
/// caller keeps ruff's default.
fn parse_python_version(s: &str) -> Option<ruff_python_ast::PythonVersion> {
    let trimmed = s.trim();
    // `py310` / `py38` → `3.10` / `3.8` (first digit is the major version).
    if let Some(rest) = trimmed.strip_prefix("py")
        && rest.len() >= 2
        && rest.chars().all(|c| c.is_ascii_digit())
    {
        let (major, minor) = rest.split_at(1);
        return format!("{major}.{minor}").parse().ok();
    }
    trimmed.parse().ok()
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
        // Suffix bumped when engine logic (not just the pinned rev) changes the
        // output for the same input: +pkgroot = package-root resolution (INP001 /
        // isort), +plugins = pydocstyle/mccabe/pylint param wiring, +e501 =
        // pycodestyle max_line_length mirrors line_length (E501 honors config),
        // +tgtsrc = target_version + src (isort roots) wiring.
        concat!(
            "git-ruff:",
            "1cb20127c47cf5c66ead93fb39e47600c857bb7e",
            "+pkgroot+plugins+isort+e501+tgtsrc"
        )
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

        // Resolve the package root from the file's directory (walk ancestors for
        // `__init__.py`) so ruff's filesystem-context checks behave as they do
        // over a real tree: without it, INP001 (implicit-namespace-package)
        // over-fires for every file in a package, and isort (I001/I002 — in the
        // default set) wrongly classifies first-party imports as third-party.
        let package = src
            .path
            .parent()
            .and_then(|parent| detect_package_root(parent, &settings.namespace_packages))
            .map(PackageRoot::root);

        let result = lint_only(
            &src.path,
            package,
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

                // Collect all edits from safe ruff fixes.  Multi-edit fixes are
                // now applied atomically by the runner (all or nothing), so it is
                // safe to forward the full edit list.  `Unsafe`/`DisplayOnly`
                // fixes are still suppressed.
                let fix: Vec<Edit> = ruff_diag
                    .fix()
                    .filter(|f| f.applicability().is_safe())
                    .map(|f| {
                        f.edits()
                            .iter()
                            .map(|e| Edit {
                                start_byte: e.start().to_usize(),
                                end_byte: e.end().to_usize(),
                                replacement: e.content().unwrap_or("").to_string(),
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                Diagnostic {
                    engine: "ruff".to_string(),
                    code,
                    severity,
                    title: message,
                    description: None,
                    span,
                    url: None,
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

#[cfg(test)]
mod tests {
    use super::parse_python_version;

    #[test]
    fn parses_ruff_style_py_prefix() {
        let py310: ruff_python_ast::PythonVersion = "3.10".parse().unwrap();
        assert_eq!(parse_python_version("py310"), Some(py310));
        let py38: ruff_python_ast::PythonVersion = "3.8".parse().unwrap();
        assert_eq!(parse_python_version("py38"), Some(py38));
    }

    #[test]
    fn parses_dotted_form() {
        let py312: ruff_python_ast::PythonVersion = "3.12".parse().unwrap();
        assert_eq!(parse_python_version("3.12"), Some(py312));
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_python_version("nonsense"), None);
        assert_eq!(parse_python_version("py"), None);
        assert_eq!(parse_python_version("pyABC"), None);
    }
}
