//! Configuration: the unified `poly.toml`, parsed
//! by the [`poly_config`] crate and sliced per-engine here. Layering is: tool
//! defaults (inside each engine) → opinionated [`GlobalDefaults`] → user config.
//!
//! `poly-core` consumes only the `[defaults]`, `[lint.*]`, and `[fmt.*]`
//! tables; the `[commit]` and `[hooks]` sections of the same file are read
//! directly from [`poly_config`] by the `poly commit` / `poly hooks` surfaces.

use std::collections::BTreeMap;
use std::path::Path;

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
#[derive(Debug, Clone)]
pub struct Config {
    /// Global opinionated defaults.
    pub defaults: GlobalDefaults,
    /// `[discovery] exclude` — gitignore-style globs pruned from the file walk
    /// on direct `poly lint`/`poly fmt`/`poly cache` runs.
    pub exclude: Vec<String>,
    /// `[discovery] force_exclude` — require API callers to apply `exclude` to
    /// explicitly named roots too. The CLI and MCP do so by default.
    pub force_exclude: bool,
    /// `[discovery] no_prune` — directory names kept despite the built-in
    /// vendored/generated prune set (see `discover::PRUNED_DIRECTORIES`).
    pub no_prune: Vec<String>,
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
    /// `[rules] builtin` — whether poly's embedded built-in ast-grep rule pack
    /// is loaded, layered beneath `rules_dirs` (a user rule wins on id
    /// conflict). Defaults to `true`.
    pub rules_builtin_pack: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            defaults: Default::default(),
            exclude: Default::default(),
            force_exclude: Default::default(),
            no_prune: Default::default(),
            lint: Default::default(),
            fmt: Default::default(),
            tools: Default::default(),
            per_file_ignores: Default::default(),
            typos_native: Default::default(),
            rules_dirs: Default::default(),
            // Matches `poly_config::RulesConfig`'s own default: the pack is
            // on unless a user explicitly turns it off.
            rules_builtin_pack: true,
        }
    }
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
    /// Load the effective config for `start`, cascading the `poly.toml` chain
    /// from the git repository root down to `start` (bounded at `.git`) when
    /// inside a repo, else the single nearest `poly.toml`. See
    /// [`poly_config::PolyConfig::load`].
    pub fn load(start: &Path) -> anyhow::Result<Config> {
        Ok(poly_config::PolyConfig::load(start)?.into())
    }

    /// [`load`](Config::load) with an explicit [`poly_config::BaseConfigResolver`]
    /// for the config's `extends` bases (ADR 0020). The default `load` resolves only
    /// local `path` bases; the CLI supplies a resolver that also fetches pinned
    /// remote git bases.
    pub fn load_with(start: &Path, resolver: &dyn poly_config::BaseConfigResolver) -> anyhow::Result<Config> {
        Ok(poly_config::PolyConfig::load_with(start, resolver)?.into())
    }

    /// Load config from an explicit file path.
    pub fn load_file(path: &Path) -> anyhow::Result<Config> {
        Ok(poly_config::PolyConfig::load_file(path)?.into())
    }

    /// [`load_file`](Config::load_file) with an explicit
    /// [`poly_config::BaseConfigResolver`] for the file's `extends` bases (ADR 0020).
    pub fn load_file_with(path: &Path, resolver: &dyn poly_config::BaseConfigResolver) -> anyhow::Result<Config> {
        Ok(poly_config::PolyConfig::load_file_with(path, resolver)?.into())
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
        } else if engine_name == "quality" {
            self.build_quality_options(&lang_options)
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

        for key in crate::engines::uncomment::BOOL_OPTION_KEYS {
            let value = lang_options
                .get(*key)
                .or_else(|| global.and_then(|table| table.get(*key)))
                .and_then(toml::Value::as_bool);
            if let Some(value) = value {
                options.insert((*key).to_string(), toml::Value::Boolean(value));
            }
        }

        for key in crate::engines::uncomment::ARRAY_OPTION_KEYS {
            let mut merged: Vec<String> = Vec::new();
            if let Some(global) = global {
                extend_string_array(&mut merged, global, key);
            }
            extend_string_array(&mut merged, lang_options, key);
            insert_string_array(&mut options, key, merged);
        }

        options
    }

    /// Build the merged `options` table for the `quality` engine.
    ///
    /// Like `uncomment`, `quality` is cross-cutting: its base config lives in
    /// a language-agnostic `[lint.quality]` table, and the per-language
    /// `[lint.<lang>.quality]` table (`lang_options`) overrides individual
    /// keys on top. Every key is a plain bool/integer/array (never a nested
    /// table), so the merge is a flat "per-language wins when present, else
    /// fall back to the global value".
    fn build_quality_options(&self, lang_options: &toml::Table) -> toml::Table {
        let global = self.lint.get("quality").and_then(toml::Value::as_table);
        let mut options = toml::Table::new();

        for key in crate::engines::quality::settings::BOOL_OPTION_KEYS {
            let value = lang_options
                .get(*key)
                .or_else(|| global.and_then(|table| table.get(*key)))
                .and_then(toml::Value::as_bool);
            if let Some(value) = value {
                options.insert((*key).to_string(), toml::Value::Boolean(value));
            }
        }

        for key in crate::engines::quality::settings::INTEGER_OPTION_KEYS {
            let value = lang_options
                .get(*key)
                .or_else(|| global.and_then(|table| table.get(*key)))
                .and_then(toml::Value::as_integer);
            if let Some(value) = value {
                options.insert((*key).to_string(), toml::Value::Integer(value));
            }
        }

        for key in crate::engines::quality::settings::ARRAY_OPTION_KEYS {
            let values = lang_options
                .get(*key)
                .or_else(|| global.and_then(|table| table.get(*key)))
                .and_then(|v| v.as_array())
                .cloned();
            if let Some(values) = values {
                options.insert((*key).to_string(), toml::Value::Array(values));
            }
        }

        options
    }

    /// Build the merged `options` table for the typos engine.
    ///
    /// Precedence (lowest → highest):
    /// 1. Native `_typos.toml` / `.typos.toml` values (`typos_native`).
    /// 2. Language-agnostic `[lint.typos]` table from `poly.toml` (poly wins on conflict).
    /// 3. Per-language `[lint.<lang>.typos]` table — the same key set as (2),
    ///    layered on top: the two maps (`extend_words` / `extend_identifiers`)
    ///    override per key, the five arrays are unioned onto the global list so a
    ///    language adds entries without dropping the shared ones.
    fn build_typos_options(&self, lang_options: &toml::Table) -> toml::Table {
        let native = &self.typos_native;
        let mut maps: Vec<(&&str, BTreeMap<String, String>)> = crate::engines::typos::MAP_OPTION_KEYS
            .iter()
            .map(|key| {
                let seed = match *key {
                    "extend_words" => native.extend_words.clone(),
                    "extend_identifiers" => native.extend_identifiers.clone(),
                    _ => BTreeMap::new(),
                };
                (key, seed)
            })
            .collect();
        let mut arrays: Vec<(&&str, Vec<String>)> = crate::engines::typos::ARRAY_OPTION_KEYS
            .iter()
            .map(|key| {
                let seed = match *key {
                    "extend_exclude" => native.extend_exclude.clone(),
                    "extend_ignore_words" => native.extend_ignore_words.clone(),
                    "extend_ignore_re" => native.extend_ignore_re.clone(),
                    "extend_ignore_words_re" => native.extend_ignore_words_re.clone(),
                    "extend_ignore_identifiers_re" => native.extend_ignore_identifiers_re.clone(),
                    _ => Vec::new(),
                };
                (key, seed)
            })
            .collect();

        let global = self.lint.get("typos").and_then(|v| v.as_table());
        for layer in global.into_iter().chain(std::iter::once(lang_options)) {
            for (key, dest) in &mut maps {
                extend_string_map(dest, layer, key);
            }
            for (key, dest) in &mut arrays {
                extend_string_array(dest, layer, key);
            }
        }

        let mut options = toml::Table::new();
        for (key, entries) in maps {
            if !entries.is_empty() {
                options.insert(
                    (*key).to_string(),
                    toml::Value::Table(entries.into_iter().map(|(k, v)| (k, toml::Value::String(v))).collect()),
                );
            }
        }
        for (key, values) in arrays {
            insert_string_array(&mut options, key, values);
        }
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
    ///
    /// Also folds in `[rules] builtin` (`builtin_pack_enabled`, always present
    /// so toggling it changes the cache key even though the pack's own content
    /// is a compile-time constant tracked by `AstGrepEngine::version()`
    /// instead) and, cross-cutting like `[lint.typos]`/`[lint.quality]`, the
    /// language-agnostic `[lint.astgrep]` table's `select` / `extend_select` /
    /// `ignore` / `rules` keys — poly's uniform rule-selection vocabulary
    /// (ADR 0016), passed through verbatim for `RuleSelection::from_options` to
    /// parse inside the engine.
    fn build_astgrep_options(&self) -> toml::Table {
        let mut options = toml::Table::new();
        if !self.rules_dirs.is_empty() {
            insert_string_array(&mut options, "rules_dirs", self.rules_dirs.clone());
            let hash = crate::engines::astgrep::rules::rules_hash(&self.rules_dirs);
            if !hash.is_empty() {
                options.insert("rules_hash".to_string(), toml::Value::String(hash));
            }
        }
        options.insert(
            "builtin_pack_enabled".to_string(),
            toml::Value::Boolean(self.rules_builtin_pack),
        );
        if let Some(astgrep_table) = self.lint.get("astgrep").and_then(|v| v.as_table()) {
            for key in ["select", "extend_select", "ignore", "rules"] {
                if let Some(value) = astgrep_table.get(key) {
                    options.insert(key.to_string(), value.clone());
                }
            }
        }
        options
    }
}

/// Merge the string entries of `table[key]` (a TOML table) into `dest`, with the
/// incoming layer winning on a duplicate key. Non-table values and non-string
/// entries are ignored.
fn extend_string_map(dest: &mut BTreeMap<String, String>, table: &toml::Table, key: &str) {
    if let Some(entries) = table.get(key).and_then(|v| v.as_table()) {
        for (name, value) in entries {
            if let Some(value) = value.as_str() {
                dest.insert(name.clone(), value.to_string());
            }
        }
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
            force_exclude: pc.discovery.force_exclude,
            no_prune: pc.discovery.no_prune.as_slice().to_vec(),
            lint: pc.lint,
            fmt: pc.fmt,
            tools: pc.tools,
            per_file_ignores: pc.per_file_ignores,
            typos_native: pc.typos_native,
            rules_builtin_pack: pc.rules.builtin,
            rules_dirs: pc.rules.dirs,
        }
    }
}

#[cfg(test)]
mod quality_options_tests {
    use super::{Config, Kind};
    use crate::language::Language;

    /// `quality` is cross-cutting like `uncomment`: a language-agnostic
    /// `[lint.quality]` table supplies the base, and `[lint.<lang>.quality]`
    /// overrides individual keys on top.
    #[test]
    fn global_quality_table_applies_when_no_per_language_override_exists() {
        let mut global = toml::Table::new();
        global.insert("function_too_long_lines".to_string(), toml::Value::Integer(40));
        let mut lint = toml::Table::new();
        lint.insert("quality".to_string(), toml::Value::Table(global));

        let config = Config {
            lint,
            ..Config::default()
        };
        let resolved = config.engine_config(&Language::Go, "quality", Kind::Lint);
        assert_eq!(
            resolved
                .options
                .get("function_too_long_lines")
                .and_then(|v| v.as_integer()),
            Some(40),
        );
    }

    #[test]
    fn per_language_quality_table_overrides_the_global_one() {
        let mut global = toml::Table::new();
        global.insert("function_too_long_lines".to_string(), toml::Value::Integer(40));
        let mut per_lang_quality = toml::Table::new();
        per_lang_quality.insert("function_too_long_lines".to_string(), toml::Value::Integer(200));
        let mut go_table = toml::Table::new();
        go_table.insert("quality".to_string(), toml::Value::Table(per_lang_quality));

        let mut lint = toml::Table::new();
        lint.insert("quality".to_string(), toml::Value::Table(global));
        lint.insert("go".to_string(), toml::Value::Table(go_table));

        let config = Config {
            lint,
            ..Config::default()
        };
        let resolved = config.engine_config(&Language::Go, "quality", Kind::Lint);
        assert_eq!(
            resolved
                .options
                .get("function_too_long_lines")
                .and_then(|v| v.as_integer()),
            Some(200),
        );
    }
}
