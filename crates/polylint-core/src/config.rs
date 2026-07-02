//! Configuration: the unified `poly.toml` (back-compat `polylint.toml`), parsed
//! by the [`poly_config`] crate and sliced per-engine here. Layering is: tool
//! defaults (inside each engine) → opinionated [`GlobalDefaults`] → user config.
//!
//! `polylint-core` consumes only the `[defaults]`, `[lint.*]`, and `[fmt.*]`
//! tables; the `[commit]` and `[hooks]` sections of the same file are read
//! directly from [`poly_config`] by the `poly commit` / `poly hooks` surfaces.

use std::collections::BTreeMap;
use std::path::Path;

// Re-exported so the rest of the crate (and downstream consumers) keep importing
// these from `polylint_core` / `crate::config` unchanged after the schema moved
// into the shared `poly-config` crate.
pub use poly_config::{GlobalDefaults, LineEnding};

use crate::language::Language;

/// Which phase a config slice is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Linting phase (`[lint.*]` tables).
    Lint,
    /// Formatting phase (`[fmt.*]` tables).
    Format,
}

/// The fully normalized configuration for the lint/format surfaces.
///
/// This is a thin projection of [`poly_config::PolyConfig`] onto the tables
/// `polylint-core` needs; the `[commit]` / `[hooks]` sections are intentionally
/// dropped here and consumed elsewhere.
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// Global opinionated defaults.
    pub defaults: GlobalDefaults,
    /// `[discovery] exclude` — gitignore-style globs pruned from the file walk
    /// on direct `poly lint`/`poly fmt`/`poly cache` runs.
    pub exclude: Vec<String>,
    /// `[lint.<lang>.<tool>]` tables.
    pub lint: toml::Table,
    /// `[fmt.<lang>.<tool>]` tables.
    pub fmt: toml::Table,
    /// `[tools.<name>]` — opted-in vendored catalog tools (ADR 0013).
    pub tools: poly_config::ToolsConfig,
    /// `[per-file-ignores]` — path glob → rule codes suppressed for matching
    /// files (lint-only). Applied as a post-lint filter on `Diagnostic.code`.
    pub per_file_ignores: BTreeMap<String, Vec<String>>,
    /// Native `_typos.toml` / `.typos.toml` configuration discovered near the config root.
    pub typos_native: poly_config::TyposNative,
}

/// The slice of config handed to one engine for one file.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Global opinionated defaults.
    pub globals: GlobalDefaults,
    /// Indent width for this file's language.
    pub indent_width: usize,
    /// Tool-specific options from `[<kind>.<lang>.<engine>]` (engine merges its own defaults).
    pub options: toml::Table,
}

impl Config {
    /// Load config by searching from `start` upward for `poly.toml` (or the
    /// back-compat `polylint.toml`).
    pub fn load(start: &Path) -> anyhow::Result<Config> {
        Ok(poly_config::PolyConfig::load(start)?.into())
    }

    /// Load config from an explicit file path.
    pub fn load_file(path: &Path) -> anyhow::Result<Config> {
        Ok(poly_config::PolyConfig::load_file(path)?.into())
    }

    /// Build the [`EngineConfig`] slice for a given language + engine + phase.
    pub fn engine_config(&self, lang: &Language, engine_name: &str, kind: Kind) -> EngineConfig {
        let tables = match kind {
            Kind::Lint => &self.lint,
            Kind::Format => &self.fmt,
        };
        let lang_options = tables
            .get(lang.id())
            .and_then(|v| v.as_table())
            .and_then(|t| t.get(engine_name))
            .and_then(|v| v.as_table())
            .cloned()
            .unwrap_or_default();
        // A user `indent_width` in the per-engine table overrides the language
        // default, uniformly for every engine (each reads `cfg.indent_width`).
        let indent_width = lang_options
            .get("indent_width")
            .and_then(toml::Value::as_integer)
            .and_then(|v| usize::try_from(v).ok())
            .filter(|&v| v > 0)
            .unwrap_or_else(|| lang.default_indent_width());
        let options = if engine_name == "typos" {
            self.build_typos_options(&lang_options)
        } else {
            lang_options
        };
        EngineConfig {
            globals: self.defaults.clone(),
            indent_width,
            options,
        }
    }

    /// Build the merged `options` table for the typos engine.
    ///
    /// Precedence (lowest → highest):
    /// 1. Native `_typos.toml` / `.typos.toml` values (`typos_native`).
    /// 2. Language-agnostic `[lint.typos]` table from `poly.toml` (poly wins on conflict).
    /// 3. Per-language `[lint.<lang>.typos]` `extend_ignore_words` (back-compat; unioned in).
    fn build_typos_options(&self, lang_options: &toml::Table) -> toml::Table {
        // Start from native file values.
        let mut extend_words: BTreeMap<String, String> = self.typos_native.extend_words.clone();
        let mut extend_identifiers: BTreeMap<String, String> = self.typos_native.extend_identifiers.clone();
        let mut extend_exclude: Vec<String> = self.typos_native.extend_exclude.clone();
        let mut extend_ignore_words: Vec<String> = vec![];

        // Overlay the language-agnostic [lint.typos] table from poly.toml.
        if let Some(poly_typos) = self.lint.get("typos").and_then(|v| v.as_table()) {
            if let Some(words) = poly_typos.get("extend_words").and_then(|v| v.as_table()) {
                for (k, v) in words {
                    if let Some(s) = v.as_str() {
                        extend_words.insert(k.clone(), s.to_string());
                    }
                }
            }
            if let Some(idents) = poly_typos.get("extend_identifiers").and_then(|v| v.as_table()) {
                for (k, v) in idents {
                    if let Some(s) = v.as_str() {
                        extend_identifiers.insert(k.clone(), s.to_string());
                    }
                }
            }
            if let Some(excl) = poly_typos.get("extend_exclude").and_then(|v| v.as_array()) {
                extend_exclude.extend(excl.iter().filter_map(|v| v.as_str()).map(|s| s.to_string()));
            }
            if let Some(words) = poly_typos.get("extend_ignore_words").and_then(|v| v.as_array()) {
                extend_ignore_words.extend(words.iter().filter_map(|v| v.as_str()).map(|s| s.to_string()));
            }
        }

        // Union per-language [lint.<lang>.typos] extend_ignore_words (back-compat).
        if let Some(per_lang_words) = lang_options.get("extend_ignore_words").and_then(|v| v.as_array()) {
            extend_ignore_words.extend(per_lang_words.iter().filter_map(|v| v.as_str()).map(|s| s.to_string()));
        }

        // Assemble the final options table.
        let mut options = toml::Table::new();
        if !extend_words.is_empty() {
            options.insert(
                "extend_words".to_string(),
                toml::Value::Table(
                    extend_words
                        .into_iter()
                        .map(|(k, v)| (k, toml::Value::String(v)))
                        .collect(),
                ),
            );
        }
        if !extend_identifiers.is_empty() {
            options.insert(
                "extend_identifiers".to_string(),
                toml::Value::Table(
                    extend_identifiers
                        .into_iter()
                        .map(|(k, v)| (k, toml::Value::String(v)))
                        .collect(),
                ),
            );
        }
        if !extend_exclude.is_empty() {
            options.insert(
                "extend_exclude".to_string(),
                toml::Value::Array(extend_exclude.into_iter().map(toml::Value::String).collect()),
            );
        }
        if !extend_ignore_words.is_empty() {
            options.insert(
                "extend_ignore_words".to_string(),
                toml::Value::Array(extend_ignore_words.into_iter().map(toml::Value::String).collect()),
            );
        }
        options
    }
}

impl From<poly_config::PolyConfig> for Config {
    fn from(pc: poly_config::PolyConfig) -> Self {
        Config {
            defaults: pc.defaults,
            exclude: pc.discovery.exclude.as_slice().to_vec(),
            lint: pc.lint,
            fmt: pc.fmt,
            tools: pc.tools,
            per_file_ignores: pc.per_file_ignores,
            typos_native: pc.typos_native,
        }
    }
}
