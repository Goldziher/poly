//! PHP backend: lint + format via the `mago` toolchain (carthage-software/mago,
//! MIT OR Apache-2.0).
//!
//! # Lint (`lint.rs`)
//! Parses the file with `mago-syntax`, resolves names with `mago-names`, then
//! runs every enabled `mago-linter` rule. Parse errors in `program.errors` are
//! surfaced first (code `"syntax"` / `"parse"`, [`crate::engine::Severity`]
//! `::Error`), followed by the linter's `mago_reporting::IssueCollection`
//! mapped to poly [`Diagnostic`]s. Where a lint issue carries exactly one
//! safe `mago_text_edit::TextEdit`, a [`crate::engine::Edit`] fix is wired so
//! `--fix` can apply it.
//!
//! # Format (`format.rs`)
//! Delegates to [`mago_formatter::Formatter`] with settings derived from
//! [`EngineConfig`]: `line_length → print_width`, `indent_width → tab_width`.
//! Returns [`FormatOutput::Unchanged`] when the formatted output equals the
//! input or when parsing fails (the lint pass surfaces the parse error instead).
//!
//! # PHP version
//! Defaults to PHP 8.4, the latest stable version at the time of writing.
//!
//! # Capabilities
//! Lint, format, and single-edit safe autofixes.
//!
//! # Registry caching
//! [`mago_linter::Linter::new`] rebuilds the rule registry on every call.
//! Because `plan_engines` constructs one [`MagoEngine`] instance per language
//! per run, and all files for that language share the same resolved
//! [`EngineConfig`], the registry can be built once and reused.
//! [`MagoEngine`] holds an [`OnceLock`] that fires on the first `lint` call
//! and hands the cached [`Arc<RuleRegistry>`] to every subsequent call via
//! [`mago_linter::Linter::from_registry`].

mod format;
mod lint;
pub(super) mod rules;

use std::sync::{Arc, OnceLock};

use mago_linter::registry::RuleRegistry;

use crate::config::EngineConfig;
use crate::engine::{Capabilities, Diagnostic, Engine, FormatOutput, OptionKeys, OptionTable, OptionType, SourceFile};
use crate::language::Language;

/// PHP backend using the `mago` linter + formatter.
///
/// Holds a lazily-initialised rule registry so the expensive
/// [`RuleRegistry::build`] step runs at most once per engine instance.
/// Each engine instance is created once per language per run by `plan_engines`,
/// so the `OnceLock` fires once per run.
pub struct MagoEngine {
    /// Lazily-built rule registry, shared across all `lint` calls on this instance.
    pub(super) registry: OnceLock<Arc<RuleRegistry>>,
}

impl Default for MagoEngine {
    fn default() -> Self {
        Self {
            registry: OnceLock::new(),
        }
    }
}

static LANGUAGES: &[Language] = &[Language::Php];

impl Engine for MagoEngine {
    fn name(&self) -> &'static str {
        "mago"
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

    /// Cache-key version: bump whenever mago output could change.
    ///
    /// `+advisory-sev1`: the `Maintainability` metric rules report `Warning`
    /// instead of mago's `Error` (see `lint::ADVISORY_RULE_CODES`).
    /// `[fmt.php.mago]`'s keys are the fields of mago's own
    /// `RawFormatSettings`, derived from that type rather than copied: the list
    /// is 96 kebab-case entries that change with every mago release, and a
    /// hand-written copy would drift on the first upgrade.
    fn option_keys(&self, table: OptionTable) -> OptionKeys {
        match table {
            OptionTable::Lint => {
                OptionKeys::declared(&[("php_version", OptionType::STRING), ("integrations", OptionType::ARRAY)])
                    .with_rule_selection()
            }
            OptionTable::Format => OptionKeys::declared(&[("php_version", OptionType::STRING)]).with_derived(
                crate::engines::config_keys::recognized_by_deserialize::<mago_formatter::settings::RawFormatSettings>,
            ),
            OptionTable::CrossCuttingLint => OptionKeys::UNCHECKED,
        }
    }

    fn version(&self) -> &str {
        "mago-1.47.4+advisory-sev1"
    }

    fn lint(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        lint::lint_php(src, cfg, &self.registry)
    }

    fn format(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
        format::format_php(src, cfg)
    }
}
