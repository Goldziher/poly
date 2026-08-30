//! oxc backend (M2): JS, TS, JSX, TSX lint + format via `oxc_linter` /
//! `oxc_formatter`, plus JSON/JSONC format via `oxc_formatter_json`.
//!
//! Lint path uses `oxc_linter` (oxlint) to run the full correctness rule set
//! in-process via `LintService::run_source`. An in-memory `RuntimeFileSystem`
//! adapter feeds file content from RAM — no disk read inside the engine.
//!
//! `oxc_formatter` (Prettier-compatible, v0.56.0) handles JS/TS formatting.
//! `oxc_formatter_json` handles JSON/JSONC formatting: Prettier-compatible,
//! short arrays stay inline, JSONC comments are preserved.
//!
//! # Module layout
//! * `lint` — oxlint diagnostics (JS/TS) + strict JSON/JSONC validation.
//! * `format` — `oxc_formatter` (JS/TS) and `oxc_formatter_json` (JSON/JSONC).
//! * `config` — building the formatter option structs from [`EngineConfig`].

mod config;
mod format;
mod lint;

use crate::config::EngineConfig;
use crate::engine::{Capabilities, Diagnostic, FormatOutput, OptionKeys, OptionTable, OptionType, SourceFile};
use crate::language::Language;

/// The formatter keys `[fmt.<lang>.oxc]` reads, with the type each is read as.
///
/// Shared with the markup_fmt backend rather than copied: an Astro `<script>`
/// block is formatted by `oxc::config` out of markup_fmt's *own* table, so the
/// two tables read the same keys and a second list would drift.
pub(crate) const JS_FORMAT_OPTION_KEYS: &[(&str, OptionType)] = &[
    ("indent_style", OptionType::STRING),
    ("quote_style", OptionType::STRING),
    ("jsx_quote_style", OptionType::STRING),
    ("semicolons", OptionType::STRING),
    ("trailing_commas", OptionType::STRING),
    ("arrow_parentheses", OptionType::STRING),
    ("bracket_spacing", OptionType::BOOLEAN),
    ("bracket_same_line", OptionType::BOOLEAN),
];

pub(crate) use self::format::{format_embedded_js, is_embedded_js_parse_error};
use self::format::{format_js, format_json};
use self::lint::{lint_js, lint_json};

/// Version string folded into the blake3 cache key.
/// Bump whenever the output of `lint` or `format` could change.
/// Reflects the oxc monorepo rev + formatter version + oxlint integration marker.
/// `+rules-v2`: per-rule `AllowWarnDeny::Deny` severity support added.
/// `+fmt-opts`:  JS quote_style, semicolons, trailing_commas, arrow_parentheses,
///               bracket_spacing, bracket_same_line, indent_style; JSON bracket_spacing
///               and trailing_commas now wired from `cfg.options`.
/// `+rules-v4`: `complexity` added to the default filters, `max-classes-per-file`
///              moved to the default allow list.
/// `+rules-v5`: `[rules.<id>]` per-rule tool-specific parameters (e.g.
///              `[rules.max-params] max = 6`) now reach the rule via a synthesized
///              `Oxlintrc` built through `ConfigStoreBuilder::from_oxlintrc`,
///              instead of being silently discarded.
const VERSION: &str =
    "oxc_formatter:0.65.0+oxlint+parser:0.147.0+rev:db66f58+json-fmt+rules-v5+fmt-opts+jsonc-trailing-comma";

static LANGUAGES: &[Language] = &[
    Language::JavaScript,
    Language::TypeScript,
    Language::Jsx,
    Language::Tsx,
    Language::Json,
    Language::Jsonc,
];

/// oxc backend: wraps `oxc_linter` for full correctness-rule lint diagnostics,
/// `oxc_formatter` for JS/TS formatting (Prettier-compatible), and
/// `oxc_formatter_json` for JSON/JSONC formatting.
pub struct OxcEngine;

impl crate::engine::Engine for OxcEngine {
    fn name(&self) -> &'static str {
        "oxc"
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

    /// `[lint.<lang>.oxc]` takes only the uniform rule vocabulary (oxlint's own
    /// `.oxlintrc.json` keys are not accepted); `[fmt.<lang>.oxc]` takes the
    /// formatter keys — the union of the JS and JSON paths, which share one
    /// table name. `line_width` is not among them: it comes from
    /// `[defaults] line_length`.
    fn option_keys(&self, table: OptionTable) -> OptionKeys {
        match table {
            OptionTable::Lint => OptionKeys::declared(&[]).with_rule_selection(),
            OptionTable::Format => OptionKeys::declared(JS_FORMAT_OPTION_KEYS),
            OptionTable::CrossCuttingLint => OptionKeys::UNCHECKED,
        }
    }

    fn version(&self) -> &str {
        VERSION
    }

    fn lint(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        match src.language {
            Language::Json | Language::Jsonc => lint_json(src),
            _ => lint_js(src, cfg),
        }
    }

    fn format(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
        match src.language {
            Language::Json | Language::Jsonc => format_json(src, cfg),
            _ => format_js(src, cfg),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;

    #[test]
    fn engine_metadata() {
        let engine = OxcEngine;
        assert_eq!(engine.name(), "oxc");
        assert!(engine.capabilities().lint);
        assert!(engine.capabilities().format);
        assert!(!engine.capabilities().fix);
    }
}
