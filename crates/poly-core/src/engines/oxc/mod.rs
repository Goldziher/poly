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
//! * `plugins` — the oxlint plugin set `[lint.<lang>.oxc] plugins` enables.

mod config;
mod external;
mod format;
mod json_lint;
mod lint;
mod native_lint;
mod plugins;
mod sort_imports;

use crate::config::EngineConfig;
use crate::engine::{Capabilities, Diagnostic, FormatOutput, OptionKeys, OptionTable, OptionType, SourceFile};
use crate::language::Language;

/// The lint keys `[lint.<lang>.oxc]` reads beyond the uniform rule vocabulary.
///
/// `plugins` names oxlint plugins to enable on top of its default set
/// (`unicorn | typescript | oxc`) — the only way to reach a rule from a plugin
/// that ships disabled, since rule selection cannot enable one. See
/// `plugins::extra_lint_plugins`.
///
/// `max_line_bytes` is the length past which a line is over-long; a file with
/// most of its bytes on such lines is minified output and left unlinted
/// (default [`lint::DEFAULT_MAX_LINE_BYTES`]; `0` lints every file). Minified bundles
/// are not authored code: their findings (single-letter shadowing, ...) are
/// noise, and a megabyte of them costs seconds.
///
/// `oxlintrc` names the project's `.oxlintrc.json[c]` (set by poly when it finds one); `false`
/// switches that off and returns to poly's own rule set.
///
/// `js_plugins` says what to do with the config's `jsPlugins`, JavaScript modules poly has no runtime
/// for: `"skip"` (default) lints without them and warns once per config; `"external"` runs the real
/// `oxlint` (from the nearest `node_modules/.bin`, else `PATH`) over the files those configs govern,
/// one run per config, and reports its findings.
const JS_LINT_OPTION_KEYS: &[(&str, OptionType)] = &[
    ("plugins", OptionType::ARRAY),
    ("max_line_bytes", OptionType::INTEGER),
    ("oxlintrc", OptionType::STRING.or(OptionType::BOOLEAN)),
    ("js_plugins", OptionType::STRING),
];

// The keys of `[fmt.<lang>.oxc]` shared with the markup_fmt backend, written once so the
// oxc-only extras below cannot drift from them.
macro_rules! js_format_keys {
    ($($extra:tt)*) => {
        &[
            ("indent_style", OptionType::STRING),
            ("quote_style", OptionType::STRING),
            ("jsx_quote_style", OptionType::STRING),
            ("semicolons", OptionType::STRING),
            ("trailing_commas", OptionType::STRING),
            ("arrow_parentheses", OptionType::STRING),
            ("bracket_spacing", OptionType::BOOLEAN),
            ("bracket_same_line", OptionType::BOOLEAN),
            ("line_width", OptionType::INTEGER),
            ("line_ending", OptionType::STRING),
            ("quote_properties", OptionType::STRING),
            ("object_wrap", OptionType::STRING),
            ("operator_position", OptionType::STRING),
            ("single_attribute_per_line", OptionType::BOOLEAN),
            ("single_quote", OptionType::BOOLEAN),
            ("sort_imports", OptionType::TABLE.or(OptionType::BOOLEAN)),
            ("sort_package_json", OptionType::TABLE.or(OptionType::BOOLEAN)),
            $($extra)*
        ]
    };
}

/// The formatter keys `[fmt.<lang>.oxc]` reads, with the type each is read as.
///
/// Shared with the markup_fmt backend rather than copied: an Astro `<script>`
/// block is formatted by `oxc::config` out of markup_fmt's *own* table, so the
/// two tables read the same keys and a second list would drift.
pub(crate) const JS_FORMAT_OPTION_KEYS: &[(&str, OptionType)] = js_format_keys!();

/// [`JS_FORMAT_OPTION_KEYS`] plus the keys only this backend reads: poly names the governing
/// `.oxfmtrc.json[c]`, flags files that need oxfmt's JavaScript-side options, and `external`
/// hands those to the real `oxfmt`.
const OXC_FORMAT_OPTION_KEYS: &[(&str, OptionType)] = js_format_keys!(
    ("oxfmtrc", OptionType::STRING),
    ("requires_oxfmt", OptionType::BOOLEAN),
    ("external", OptionType::BOOLEAN),
);

pub(crate) use self::format::{format_embedded_js, is_embedded_js_parse_error};

/// `oxc_formatter` options for a JS/TS block embedded in another document (a Markdown fence).
pub(crate) fn embedded_js_options(cfg: &EngineConfig) -> oxc_formatter::JsFormatOptions {
    config::build_js_options(cfg)
}

/// `oxc_formatter_json` options for a JSON block embedded in another document.
pub(crate) fn embedded_json_options(
    cfg: &EngineConfig,
    variant: oxc_formatter_json::JsonVariant,
) -> oxc_formatter_json::JsonFormatOptions {
    let anonymous = SourceFile {
        path: std::path::PathBuf::new(),
        language: Language::Json,
        content: "".into(),
    };
    let mut options = config::build_json_options(&anonymous, cfg);
    options.variant = variant;
    options
}
use self::format::{format_js, format_json};
use self::json_lint::lint_json;
use self::lint::lint_js;

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
/// `+plugins-v1`: `[lint.<lang>.oxc] plugins` enables oxlint plugins outside the
///                default set, so a file can now report rules (e.g.
///                `vitest/expect-expect`) that no configuration could reach
///                before.
/// `+oxlintrc-v1`: with a project `.oxlintrc.json[c]`, JS/TS lint runs that config (nested configs
///                 included) instead of poly's rule set.
/// `+package-json-v1`: `package.json`, `composer.json` and import maps use the
///                     `json-stringify` variant; `sort_package_json` sorts `package.json` keys.
/// `+oxfmtrc-v1`: `line_width`, `line_ending`, `quote_properties`, `object_wrap`,
///                `operator_position`, `single_attribute_per_line` and `sort_imports`
///                reach the formatters, and `.oxfmtrc.json[c]` supplies them by default.
const VERSION: &str = "oxc_formatter:0.75.0+oxlint+parser:0.156.0+rev:e2be72d+json-fmt+rules-v5+fmt-opts+\
                       jsonc-trailing-comma+plugins-v1+minified-skip+oxfmtrc-v1+package-json-v1+oxlintrc-v1+rule-args-1+safe-fix-1+js-plugins-ext-1+oxfmtrc-scope-1";

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

    /// `[lint.<lang>.oxc]` takes the uniform rule vocabulary plus `plugins`
    /// (oxlint's other `.oxlintrc.json` keys are not accepted);
    /// `[fmt.<lang>.oxc]` takes the formatter keys — the union of the JS and
    /// JSON paths, which share one table name. `line_width` is not among them:
    /// it comes from `[defaults] line_length`.
    fn option_keys(&self, table: OptionTable) -> OptionKeys {
        match table {
            OptionTable::Lint => OptionKeys::declared(JS_LINT_OPTION_KEYS).with_rule_selection(),
            OptionTable::Format => OptionKeys::declared(OXC_FORMAT_OPTION_KEYS),
            OptionTable::CrossCuttingLint => OptionKeys::UNCHECKED,
        }
    }

    fn version(&self) -> &str {
        VERSION
    }

    /// Digest of the oxlint configs governing `path`, when the project has any: they differ per
    /// directory, so the options table alone cannot say which rules ran.
    fn lint_input_salt(&self, path: &std::path::Path, cfg: &EngineConfig) -> Option<String> {
        native_lint::governing_for(cfg, path).map(|governing| governing.digest.clone())
    }

    fn format_config<'a>(&self, src: &SourceFile, cfg: &'a EngineConfig) -> std::borrow::Cow<'a, EngineConfig> {
        crate::oxfmt_scope::path_config(cfg, src, self.name())
    }

    fn format_input_salt(&self, path: &std::path::Path, cfg: &EngineConfig) -> Option<String> {
        crate::oxfmt_scope::input_salt(cfg, path)
    }

    /// Both digests: a cached lint verdict goes stale with the oxlint config, a cached format
    /// result with the oxfmt one.
    fn file_context(&self, path: &std::path::Path, cfg: &EngineConfig) -> Option<String> {
        match (self.lint_input_salt(path, cfg), self.format_input_salt(path, cfg)) {
            (None, None) => None,
            (lint, format) => Some(format!("{}|{}", lint.unwrap_or_default(), format.unwrap_or_default())),
        }
    }

    fn lint(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        match src.language {
            Language::Json | Language::Jsonc => lint_json(src),
            _ => lint_js(src, cfg),
        }
    }

    fn batch_support(&self, cfg: &EngineConfig) -> crate::engine::BatchSupport {
        crate::engine::BatchSupport {
            format: false,
            lint: external::lint_requested(cfg),
        }
    }

    fn lint_batch(
        &self,
        batch: &[SourceFile],
        cfg: &EngineConfig,
    ) -> anyhow::Result<Vec<anyhow::Result<Vec<Diagnostic>>>> {
        external::lint_batch(batch, cfg, |src| self.lint(src, cfg))
    }

    fn batch_shard_floor(&self) -> usize {
        256
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
