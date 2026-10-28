//! Python backend: full rule-based linting via `ruff_linter` and formatting
//! via `ruff_python_formatter`.
//!
//! Both come from the published ruff crates on crates.io, pinned exactly
//! (`ruff_linter = "=0.17.0"`, the rest `=0.0.17`; see the workspace
//! `Cargo.toml`). The `=` is deliberate: these are private-by-intent internals
//! that ruff does not treat as a public API, so a caret range would let
//! `cargo update` walk into unannounced breakage between releases. The version
//! is folded into [`RuffEngine::version`] so that upgrading it automatically
//! invalidates the poly cache.
//!
//! # Opinionated rule selection
//!
//! The default selection extends ruff's built-in defaults (F + E4/E7/E9) into an
//! opinionated modern-Python set — see [`RULE_CODES`] for the full list with a
//! per-category rationale, and [`DEFAULT_IGNORED_RULES`] for the individual
//! members held back because they fire on legitimate code.
//!
//! `E1`/`E2`/`E3`/`W1`/`W2`/`W3` are intentionally excluded — they overlap
//! with the ruff formatter and would fire on every well-formatted file.
//! Preview rules stay off unless `preview = true` is set in `[lint.python.ruff]`.
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
//! `[lint.python.ruff]` table in the user's `poly.toml`. On the format side the
//! recognized keys are `line_length`, `indent_width`, `docstring_code_format`,
//! and `docstring_code_line_length` (an integer, or `"dynamic"`); see
//! [`build_format_options`].

use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use ruff_db::diagnostic::Severity as RuffSeverity;
use ruff_linter::linter::{ParseSource, lint_only};
use ruff_linter::package::PackageRoot;
use ruff_linter::packaging::detect_package_root;
use ruff_linter::rule_selector::{PreviewOptions, RuleSelector};
use ruff_linter::rules::pydocstyle::settings::Convention as PydocstyleConvention;
use ruff_linter::settings::LinterSettings;
use ruff_linter::settings::flags;
use ruff_linter::settings::rule_table::RuleTable;
use ruff_linter::source_kind::SourceKind;
use ruff_python_ast::PySourceType;
use ruff_text_size::Ranged;

mod context;
mod format;
mod option_keys;
mod plugins;

use crate::config::EngineConfig;
use crate::engine::{
    Capabilities, Diagnostic, Edit, Engine, FormatOutput, OptionKeys, OptionTable, Severity, SourceFile, Span,
};
use crate::language::Language;

/// Opinionated rule selection: string codes resolved by [`RuleSelector::from_str`].
///
/// Extends ruff's built-in defaults (F + E4/E7/E9). Omits E1/E2/E3/W1/W2/W3
/// because the ruff formatter already handles those whitespace/blank-line rules,
/// and keeps preview rules off unless the user sets `preview = true`.
///
/// Individual members of a selected category that fire on legitimate code are
/// turned back off in [`DEFAULT_IGNORED_RULES`], each with the measurement that
/// justifies it — "category on, one member off" is the mechanism, not a narrower
/// prefix.
static RULE_CODES: &[&str] = &[
    // ── correctness (ruff's own default set, plus escapes and import order) ──
    "F",  // Pyflakes: undefined names, unused imports
    "E4", // pycodestyle: import errors (E401/E402)
    "E7", // pycodestyle: statement errors (E711…E743) — includes E722 bare-except
    "E9", // pycodestyle: runtime/syntax errors (E999)
    "W6", // pycodestyle: W605 invalid escape sequence
    "I",  // isort: import sorting
    "UP", // pyupgrade: modernize Python syntax
    "B",  // flake8-bugbear: common bugs and design issues
    // ── typing: every function signature carries types ──
    "ANN", // flake8-annotations (ANN101/ANN102 were removed upstream in ruff 0.8.0)
    // ── functional style: prefer expressions and comprehensions over statements ──
    "SIM",  // flake8-simplify: collapsible ifs, redundant bool conversions
    "C4",   // flake8-comprehensions: comprehension over map/filter/list() round-trips
    "RET",  // flake8-return: unnecessary else/assign before return
    "FURB", // refurb: modern-Python idiom upgrades
    "PERF", // perflint: avoidable per-iteration work in loops
    // ── error handling: the best-evidenced AI defect class (CWE-248 / CWE-390) ──
    "TRY",  // tryceratops: raise/except discipline
    "BLE",  // flake8-blind-except: `except Exception:` with no re-raise
    "S110", // flake8-bandit: `try: … except: pass` — a silently swallowed error
    // ── complexity: makes the already-wired mccabe/pylint thresholds live ──
    "C90", // mccabe: cyclomatic complexity (threshold: `mccabe_max_complexity`)
    "PLR", // Pylint refactor, incl. PLR09xx too-many-args/branches/returns/statements
    // ── hygiene ──
    "T20", // flake8-print: stray print()/pprint() left in shipped code
    "TC",  // flake8-type-checking: imports that belong in a TYPE_CHECKING block
    "PTH", // flake8-use-pathlib: os.path calls that pathlib does better
    "RUF", // ruff's own rules
    "ARG", // flake8-unused-arguments
];

/// Rules disabled by default even though their category is selected above.
///
/// Every entry was measured against a six-repository Python/TypeScript corpus
/// (151 Python files) and hand-read before being turned off; the finding count
/// quoted for each is from that run.
///
/// Re-enable any of them with `extend_select = ["<code>"]` (or an explicit
/// `select`) in `[lint.python.ruff]`.
///
/// - **B008** (function-call in an argument default) fires on the idiomatic
///   FastAPI / typer / Click dependency-injection pattern
///   (`def handler(user = Depends(...))`, `x = Query(...)`, `arg = Argument(...)`),
///   where the call in the default is deliberate, not a bug. The rest of
///   flake8-bugbear (notably B006, mutable default arguments) catches real bugs
///   and stays on; only this false-positive-prone member is off.
/// - **RUF100** (unused `noqa`) fires on every `# noqa: X` whose code poly's
///   default set does not select — it measures the distance between the file's
///   own ruff config and poly's, not anything about the code. 11,570 findings,
///   76 per Python file, and 99% of them read `(non-enabled: …)`.
/// - **ANN002 / ANN003** (missing annotation for `*args` / `**kwargs`) — 2,247
///   findings, ~7.4 per file. The only annotation that ever satisfies them is
///   `Any`, which ANN401 (which *is* on) would then flag; the pair adds no type
///   information, it just moves the finding.
/// - **ARG002** (unused method argument) — 2,228 findings, every single sampled
///   one a `*args`/`**kwargs` in an override or callback whose signature is
///   fixed by its caller. ARG001/ARG003/ARG004/ARG005 stay on.
/// - **PLR2004** (magic value in comparison) — 1,127 findings across all five
///   Python repos, overwhelmingly HTTP status codes in tests
///   (`assert response.status_code == 200`). Naming those is not an improvement.
/// - **TRY003** (long message outside the exception class) — 77 findings across
///   all five repos. Satisfying it means declaring an exception subclass for
///   every `raise ValueError("…")`; no defect is detected either way.
///
/// **ANN401** (`Any` is disallowed) is deliberately *not* on this list. It was
/// measured at 97 findings / 0.64 per Python file — an order of magnitude below
/// everything above — and a hand-read of 20 random hits split 11 genuinely-
/// correct `Any` (typing introspection: `annotation: Any`, `**kwargs: Any` in a
/// passthrough) against 9 narrowable ones (`coro: Any` for a
/// `Coroutine[Any, Any, T]`, `native: Any` for a binding object the `.pyi`
/// already names). None was ruff's documented `MyAny = Any` type-alias false
/// positive. It is also the direct Python analogue of the
/// `typescript/no-explicit-any` poly enables for oxlint, so switching one off
/// while shipping the other would be incoherent. Suppress it per-site.
///
/// The `EM` category (exception message assigned to a variable before the
/// `raise`) was measured at 77 findings / 0.51 per file across all five repos
/// and is not selected at all — see [`RULE_CODES`].
static DEFAULT_IGNORED_RULES: &[&str] = &["B008", "RUF100", "ANN002", "ANN003", "ARG002", "PLR2004", "TRY003"];

/// The effective default ignore set: [`DEFAULT_IGNORED_RULES`] minus every rule
/// the user's own selection covers, by exact code, by prefix (`RUF`) or by `ALL`.
///
/// `user_codes` is what the user asked for, not poly's defaults: an explicit
/// `select` replaces poly's opinionated set and with it the reason to hold
/// individual members back, while `extend_select` adds to a set whose held-back
/// members stay held back unless it names them by their full code.
fn default_ignored(user_codes: &[String], preview: &PreviewOptions) -> Vec<String> {
    let enabled = rules_for_codes(user_codes, preview);
    DEFAULT_IGNORED_RULES
        .iter()
        .filter(|code| {
            let rules = rules_for_codes(&[code.to_string()], preview);
            !rules.iter().any(|rule| enabled.contains(rule))
        })
        .map(|code| code.to_string())
        .collect()
}

/// Resolve a list of rule-code strings to ruff `Rule`s.
fn rules_for_codes(codes: &[String], preview: &PreviewOptions) -> Vec<ruff_linter::registry::Rule> {
    codes
        .iter()
        .filter_map(|s| match RuleSelector::from_str(s) {
            Ok(selector) => Some(selector),
            Err(_) => {
                tracing::warn!(code = %s, engine = "ruff", "unknown rule or category; skipping");
                None
            }
        })
        .flat_map(|sel| sel.rules(preview).collect::<Vec<_>>())
        .collect()
}

/// Build a [`RuleTable`] from a selected set of codes minus an ignored set.
fn build_rule_table(select: &[String], ignore: &[String], preview: &PreviewOptions) -> RuleTable {
    let selected = rules_for_codes(select, preview);
    let ignored = rules_for_codes(ignore, preview);
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
        settings.rules = build_rule_table(
            &codes,
            &default_ignored(&[], &PreviewOptions::default()),
            &PreviewOptions::default(),
        );
        settings.line_length =
            ruff_linter::line_width::LineLength::try_from(120_u16).expect("120 is a valid line length");
        settings.pycodestyle.max_line_length = settings.line_length;
        settings
    })
}

/// [`build_settings`], computed once per distinct *resolved* engine config for the process.
///
/// `cfg` is the config resolved for the file's own directory (nested `poly.toml`
/// files and their `inherit_mode` cascade already applied), so two directories
/// with different ruff options never share an entry.
///
/// A run lints thousands of files under a handful of configs, and building the
/// settings expands every selected rule family into a rule table — about a
/// quarter of a file's lint time when rebuilt per file. The key is the config's
/// serialized options plus the global line length, which is everything
/// `build_settings` reads.
fn settings_for(cfg: &EngineConfig) -> Arc<LinterSettings> {
    static MEMO: OnceLock<Mutex<HashMap<String, Arc<LinterSettings>>>> = OnceLock::new();
    let key = format!(
        "{}\u{0}{}",
        cfg.globals.line_length,
        toml::to_string(&cfg.options).unwrap_or_default()
    );
    let memo = MEMO.get_or_init(Mutex::default);
    if let Some(hit) = memo.lock().unwrap_or_else(PoisonError::into_inner).get(&key) {
        return Arc::clone(hit);
    }
    let built = Arc::new(build_settings(cfg));
    memo.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(key, Arc::clone(&built));
    built
}

/// Build [`LinterSettings`] from user config, layered over the opinionated base.
///
/// Honors `[lint.python.ruff]` keys: `select` (replaces the default rule set),
/// `extend_select` (adds to it), `ignore` (removes rules), and `line_length`
/// (overriding the global default — only affects line-length rules, which the
/// default set omits). Called only when config options are present; the empty
/// case uses [`default_settings`] to avoid rebuilding per file.
fn build_settings(cfg: &EngineConfig) -> LinterSettings {
    let selection = super::rule_config::RuleSelection::from_options(cfg);

    let preview = plugins::preview_options(cfg);
    let replaces_defaults = !selection.select.is_empty();
    let mut codes: Vec<String> = if replaces_defaults {
        selection.select
    } else {
        RULE_CODES.iter().map(|s| (*s).to_owned()).collect()
    };
    let user_codes: Vec<String> = if replaces_defaults {
        codes.iter().chain(&selection.extend_select).cloned().collect()
    } else {
        selection
            .extend_select
            .iter()
            .filter(|code| rules_for_codes(std::slice::from_ref(code), &preview).len() == 1)
            .cloned()
            .collect()
    };
    codes.extend(selection.extend_select);
    // Fold in the default ignore set (e.g. B008), minus what the user's own
    // selection covers.
    let mut ignore = selection.ignore;
    ignore.extend(default_ignored(&user_codes, &preview));

    let line_length = cfg
        .options
        .get("line_length")
        .and_then(toml::Value::as_integer)
        .map(|v| v as usize)
        .unwrap_or(cfg.globals.line_length);

    let mut settings = LinterSettings::new(Path::new("."));
    settings.preview = preview.mode;
    settings.explicit_preview_rules = preview.require_explicit;
    settings.rules = build_rule_table(&codes, &ignore, &preview);
    settings.line_length = u16::try_from(line_length)
        .ok()
        .and_then(|w| ruff_linter::line_width::LineLength::try_from(w).ok())
        .unwrap_or_else(|| ruff_linter::line_width::LineLength::try_from(120_u16).expect("120 is valid"));
    settings.pycodestyle.max_line_length = settings.line_length;

    if let Some(v) = cfg
        .options
        .get("mccabe_max_complexity")
        .and_then(toml::Value::as_integer)
        .and_then(|v| usize::try_from(v).ok())
    {
        settings.mccabe.max_complexity = v;
    }

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

    if let Some(version) = cfg
        .options
        .get("target_version")
        .and_then(toml::Value::as_str)
        .and_then(parse_python_version)
    {
        settings.unresolved_target_version = version.into();
    }

    plugins::apply(&mut settings, cfg);

    settings
}

/// Parse a Python target version from either ruff's canonical `py310` spelling
/// or the dotted `3.10` form. Returns `None` for anything unrecognised so the
/// caller keeps ruff's default.
fn parse_python_version(s: &str) -> Option<ruff_python_ast::PythonVersion> {
    let trimmed = s.trim();
    if let Some(rest) = trimmed.strip_prefix("py")
        && rest.len() >= 2
        && rest.chars().all(|c| c.is_ascii_digit())
    {
        let (major, minor) = rest.split_at(1);
        return format!("{major}.{minor}").parse().ok();
    }
    trimmed.parse().ok()
}

/// Convert a ruff [`RuffSeverity`] to the poly [`Severity`].
fn map_severity(s: RuffSeverity) -> Severity {
    match s {
        RuffSeverity::Hint | RuffSeverity::Info => Severity::Info,
        RuffSeverity::Warning => Severity::Warning,
        RuffSeverity::Error | RuffSeverity::Fatal => Severity::Error,
    }
}

/// Rule-code prefixes reported at [`Severity::Warning`] rather than ruff's own
/// `Error`, so they never fail a run.
///
/// `poly lint` exits non-zero only on error-severity findings, so the severity a
/// backend assigns decides whether a rule is a **gate** or a **guard rail**. Ruff
/// reports every violation at `Error`; adopting that verbatim for the opinionated
/// categories would mean a repo that upgrades poly discovers its CI is red because
/// its functions lack return annotations — a style opinion breaking a build.
///
/// So the split follows what a finding *means*, not what ruff calls it:
///
/// - **Error (not listed here)** — the correctness core poly has always shipped:
///   `F` (undefined names), `E4`/`E7`/`E9` (import, statement, and syntax errors),
///   `W6` (invalid escapes), `I` (import order), `UP` (pyupgrade), `B` (bugbear).
///   These were already `Error` before the opinionated set was added, so their
///   behaviour is unchanged.
/// - **Warning (listed here)** — the categories added for typing, functional style,
///   error handling, complexity, and hygiene. Real signal, but advisory: a
///   consumer opts into failing on them with `[lint.python.ruff.rules.<code>]
///   level = "error"`, which the per-rule remap applies after this mapping.
///
/// Matching is by prefix, the same `code_matches_rule` semantics the configured
/// remap and `[per-file-ignores]` use, so `ANN` covers `ANN001`, `ANN201`, ….
static ADVISORY_RULE_PREFIXES: &[&str] = &[
    "ANN", // typing
    "SIM", "C4", "RET", "FURB", "PERF", // functional style
    "TRY", "BLE", "S110", // error handling
    "C90", "PLR", // complexity
    "T20", "TC", "PTH", "RUF", "ARG", // hygiene
];

/// Downgrade `severity` to [`Severity::Warning`] when `code` belongs to an
/// advisory category (see [`ADVISORY_RULE_PREFIXES`]).
///
/// Only ever downgrades: a finding ruff already reports below `Error` keeps its
/// own level.
fn advisory_severity(code: Option<&str>, severity: Severity) -> Severity {
    if severity != Severity::Error {
        return severity;
    }
    let Some(code) = code else {
        return severity;
    };
    if ADVISORY_RULE_PREFIXES
        .iter()
        .any(|prefix| code.starts_with(prefix) && code[prefix.len()..].bytes().all(|b| b.is_ascii_digit()))
    {
        Severity::Warning
    } else {
        severity
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

    /// Version string incorporates the pinned ruff crates.io version so that
    /// upgrading it automatically invalidates any cached lint/format output.
    /// The suffix records poly's own resolved defaults, which change output
    /// independently of the ruff version.
    /// `[lint.python.ruff]` takes the uniform rule vocabulary plus ruff's own
    /// flat settings; `[fmt.python.ruff]` takes the three formatter keys
    /// `build_format_options` reads. The two sets are **not** interchangeable —
    /// `docstring_code_format` under `[lint…]` does nothing.
    fn option_keys(&self, table: OptionTable) -> OptionKeys {
        match table {
            OptionTable::Lint => OptionKeys::declared(option_keys::LINT).with_rule_selection(),
            OptionTable::Format => OptionKeys::declared(format::OPTION_KEYS),
            OptionTable::CrossCuttingLint => OptionKeys::UNCHECKED,
        }
    }

    fn version(&self) -> &str {
        "ruff-0.17.3+pkgroot+plugins+isort+e501+tgtsrc+ignore-b008+rules-v3+advisory-sev1+fmtopts2+plugopts2+isort3+pylint2+impconv1+defignore2+unsafefix1+plugopts3+fmtopts3"
    }

    fn run_context(&self, cfg: &EngineConfig) -> String {
        let src_roots: Vec<std::path::PathBuf> = cfg
            .options
            .get("src")
            .and_then(toml::Value::as_array)
            .map(|roots| roots.iter().filter_map(toml::Value::as_str).map(Into::into).collect())
            .unwrap_or_default();
        context::forget_package_roots();
        context::module_tree(&src_roots)
    }

    fn file_context(&self, path: &Path, _cfg: &EngineConfig) -> Option<String> {
        Some(context::run_package_root(path))
    }

    fn lint(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        let owned_settings;
        let settings = if cfg.options.is_empty() {
            default_settings()
        } else {
            owned_settings = settings_for(cfg);
            &*owned_settings
        };

        let unsafe_fixes = cfg.options.get("unsafe_fixes").and_then(toml::Value::as_bool) == Some(true);
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

        let package_dir = src.path.parent().and_then(|parent| {
            if settings.namespace_packages.is_empty() {
                context::cached_package_root(parent)
            } else {
                detect_package_root(parent, &settings.namespace_packages).map(Path::to_path_buf)
            }
        });
        let package = package_dir.as_deref().map(PackageRoot::root);

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
                let severity = advisory_severity(code.as_deref(), map_severity(ruff_diag.severity()));
                let message = ruff_diag.concise_message().to_string();

                let span = ruff_diag
                    .primary_span()
                    .and_then(|primary| {
                        let range = primary.range()?;
                        let code = primary.as_ruff_file()?.to_source_code();
                        Some((code.line_column(range.start()), code.line_column(range.end())))
                    })
                    .map(|(start, end)| Span {
                        start_line: start.line.get() as u32,
                        start_col: start.column.get() as u32,
                        end_line: end.line.get() as u32,
                        end_col: end.column.get() as u32,
                    });

                let fix: Vec<Edit> = ruff_diag
                    .fix()
                    .filter(|f| f.applicability().is_safe() || (unsafe_fixes && !f.applicability().is_display_only()))
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
        if format::excluded(&src.path, cfg) {
            return Ok(FormatOutput::Unchanged);
        }
        let options = format::build_options(src, cfg);

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
    use std::sync::Arc;

    use super::{parse_python_version, settings_for};
    use crate::config::{EngineConfig, GlobalDefaults};

    fn engine_config(options: &str) -> EngineConfig {
        EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 4,
            options: toml::from_str(options).unwrap(),
        }
    }

    #[test]
    fn settings_are_built_once_per_distinct_config() {
        let a = settings_for(&engine_config("select = [\"F\"]\nline_length = 99"));
        let same = settings_for(&engine_config("select = [\"F\"]\nline_length = 99"));
        let other = settings_for(&engine_config("select = [\"F\"]\nline_length = 100"));

        assert!(Arc::ptr_eq(&a, &same), "an identical config reuses the built settings");
        assert!(!Arc::ptr_eq(&a, &other), "a different config does not");
        assert_ne!(a.line_length, other.line_length);
    }

    #[test]
    fn directories_with_different_ruff_options_lint_with_their_own_rules() {
        use crate::engine::{Engine, SourceFile};
        use crate::language::Language;

        let source = SourceFile {
            path: "pkg/x.py".into(),
            language: Language::Python,
            content: Arc::from("import os\nvalue = 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'\n"),
        };
        let codes = |options: &str| -> Vec<String> {
            super::RuffEngine
                .lint(&source, &engine_config(options))
                .unwrap()
                .into_iter()
                .filter_map(|diagnostic| diagnostic.code)
                .collect()
        };

        assert_eq!(codes("select = [\"E501\"]\nline_length = 20"), ["E501"]);
        assert_eq!(
            codes("select = [\"F401\"]\nline_length = 20"),
            ["F401"],
            "the same global line length with different options must not share settings"
        );
        assert_eq!(codes("select = [\"E501\"]\nline_length = 20"), ["E501"]);
    }

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
