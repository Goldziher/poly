//! Configuration: the unified `poly.toml`, parsed
//! by the [`poly_config`] crate and sliced per-engine here. Layering is: tool
//! defaults (inside each engine) → opinionated [`GlobalDefaults`] → user config.
//!
//! `poly-core` consumes only the `[defaults]`, `[lint.*]`, and `[fmt.*]`
//! tables; the `[commit]` and `[hooks]` sections of the same file are read
//! directly from [`poly_config`] by the `poly commit` / `poly hooks` surfaces.

use std::collections::BTreeMap;
use std::path::Path;

// Re-exported so the rest of the crate (and downstream consumers) keep importing
// these from `poly_core` / `crate::config` unchanged after the schema moved
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
/// `poly-core` needs; the `[commit]` / `[hooks]` sections are intentionally
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
    /// `[rules] dirs` — custom ast-grep YAML rule directories.
    /// Paths are relative to the config file root; resolved absolute paths are
    /// stored here after projection from [`poly_config::PolyConfig`].
    pub rules_dirs: Vec<String>,
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
    /// Load config by searching from `start` upward for `poly.toml`.
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
        } else if engine_name == "astgrep" {
            self.build_astgrep_options()
        } else if engine_name == "uncomment" {
            self.build_uncomment_options(&lang_options)
        } else {
            lang_options
        };
        EngineConfig {
            globals: self.defaults.clone(),
            indent_width,
            options,
        }
    }

    /// Build the merged `options` table for the uncomment engine.
    ///
    /// The uncomment backend is language-agnostic (opt-in comment removal), so its
    /// base config lives in a language-agnostic `[lint.uncomment]` table (like
    /// `[lint.typos]`). The per-language `[lint.<lang>.uncomment]` table
    /// (`lang_options`) layers on top: each boolean overrides the global value, and
    /// `preserve_patterns` are *unioned* onto the global list so a language adds
    /// patterns without dropping the shared ones.
    fn build_uncomment_options(&self, lang_options: &toml::Table) -> toml::Table {
        let global = self.lint.get("uncomment").and_then(toml::Value::as_table);
        let mut options = toml::Table::new();

        // Booleans: per-language wins over global.
        for key in [
            "enabled",
            "remove_todos",
            "remove_fixme",
            "remove_docs",
            "use_default_ignores",
        ] {
            let value = lang_options
                .get(key)
                .or_else(|| global.and_then(|table| table.get(key)))
                .and_then(toml::Value::as_bool);
            if let Some(value) = value {
                options.insert(key.to_string(), toml::Value::Boolean(value));
            }
        }

        // preserve_patterns: union of global + per-language.
        let mut preserve_patterns: Vec<String> = Vec::new();
        if let Some(global) = global {
            extend_string_array(&mut preserve_patterns, global, "preserve_patterns");
        }
        extend_string_array(&mut preserve_patterns, lang_options, "preserve_patterns");
        if !preserve_patterns.is_empty() {
            options.insert(
                "preserve_patterns".to_string(),
                toml::Value::Array(preserve_patterns.into_iter().map(toml::Value::String).collect()),
            );
        }

        options
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
        let mut extend_ignore_words: Vec<String> = self.typos_native.extend_ignore_words.clone();
        let mut extend_ignore_re: Vec<String> = self.typos_native.extend_ignore_re.clone();
        let mut extend_ignore_words_re: Vec<String> = self.typos_native.extend_ignore_words_re.clone();
        let mut extend_ignore_identifiers_re: Vec<String> = self.typos_native.extend_ignore_identifiers_re.clone();

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
            extend_string_array(&mut extend_exclude, poly_typos, "extend_exclude");
            extend_string_array(&mut extend_ignore_words, poly_typos, "extend_ignore_words");
            extend_string_array(&mut extend_ignore_re, poly_typos, "extend_ignore_re");
            extend_string_array(&mut extend_ignore_words_re, poly_typos, "extend_ignore_words_re");
            extend_string_array(
                &mut extend_ignore_identifiers_re,
                poly_typos,
                "extend_ignore_identifiers_re",
            );
        }

        // Union per-language [lint.<lang>.typos] extend_ignore_words (back-compat).
        extend_string_array(&mut extend_ignore_words, lang_options, "extend_ignore_words");

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
        insert_string_array(&mut options, "extend_exclude", extend_exclude);
        insert_string_array(&mut options, "extend_ignore_words", extend_ignore_words);
        insert_string_array(&mut options, "extend_ignore_re", extend_ignore_re);
        insert_string_array(&mut options, "extend_ignore_words_re", extend_ignore_words_re);
        insert_string_array(
            &mut options,
            "extend_ignore_identifiers_re",
            extend_ignore_identifiers_re,
        );
        options
    }

    /// Build the `options` table for the `astgrep` engine.
    ///
    /// Injects the resolved `rules_dirs` so that the engine can discover rule
    /// files without needing direct access to the full [`Config`], plus a
    /// content hash of every rule file (`rules_hash`) so that editing a rule —
    /// not just changing the dirs list — invalidates the content-hash cache via
    /// `serialized_args`. `version()` is static, so this hash is what makes rule
    /// edits take effect.
    fn build_astgrep_options(&self) -> toml::Table {
        let mut options = toml::Table::new();
        if !self.rules_dirs.is_empty() {
            insert_string_array(&mut options, "rules_dirs", self.rules_dirs.clone());
            let hash = crate::engines::astgrep::rules::rules_hash(&self.rules_dirs);
            if !hash.is_empty() {
                options.insert("rules_hash".to_string(), toml::Value::String(hash));
            }
        }
        options
    }
}

/// Append the string elements of `table[key]` (a TOML array) onto `dest`.
/// Non-array values and non-string elements are ignored.
fn extend_string_array(dest: &mut Vec<String>, table: &toml::Table, key: &str) {
    if let Some(arr) = table.get(key).and_then(|v| v.as_array()) {
        dest.extend(arr.iter().filter_map(|v| v.as_str()).map(str::to_string));
    }
}

/// Insert `values` into `options` under `key` as a TOML string array, skipping
/// the insert entirely when the list is empty (keeps the options table minimal).
fn insert_string_array(options: &mut toml::Table, key: &str, values: Vec<String>) {
    if !values.is_empty() {
        options.insert(
            key.to_string(),
            toml::Value::Array(values.into_iter().map(toml::Value::String).collect()),
        );
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
            rules_dirs: pc.rules.dirs,
        }
    }
}
