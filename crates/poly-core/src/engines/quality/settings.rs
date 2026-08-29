//! Resolved `[lint.quality]` / `[lint.<lang>.quality]` configuration.
//!
//! Flat keys, mirroring the style `uncomment` uses: a boolean toggle plus a
//! numeric threshold per rule, all optional with the ADR 0027 defaults baked
//! in here. The whole options table is folded into the lint cache key by the
//! runner (same mechanism every other engine relies on), so changing any of
//! these invalidates cached results automatically.

use crate::config::EngineConfig;

/// Default `file-too-long` threshold (lines). Matches the retired
/// `scripts/hooks/rust-max-lines.sh` cap.
pub const DEFAULT_FILE_TOO_LONG: i64 = 1000;
/// Default `function-too-long` threshold (lines).
pub const DEFAULT_FUNCTION_TOO_LONG: i64 = 80;
/// Default `type-too-long` threshold (lines).
pub const DEFAULT_TYPE_TOO_LONG: i64 = 300;
/// Default `too-many-parameters` threshold (count).
pub const DEFAULT_TOO_MANY_PARAMETERS: i64 = 6;
/// Default `nesting-too-deep` threshold (depth).
pub const DEFAULT_NESTING_TOO_DEEP: i64 = 4;
/// Default `cyclomatic-complexity` threshold.
pub const DEFAULT_CYCLOMATIC_COMPLEXITY: i64 = 20;
/// Default `law-of-demeter` chain-depth threshold.
pub const DEFAULT_LAW_OF_DEMETER: i64 = 3;
/// Default `magic-number` allowlist.
pub const DEFAULT_MAGIC_NUMBER_ALLOW: &[i64] = &[-1, 0, 1, 2, 10, 100];

/// The boolean option keys `[lint.quality]` / `[lint.<lang>.quality]` accept.
///
/// One list, read twice: `Config::build_quality_options` merges exactly these
/// keys out of the user's tables, and `QualityEngine::option_keys` declares them
/// as what the backend reads. A key present in one place and absent from the
/// other is the defect this file exists to make impossible.
pub(crate) const BOOL_OPTION_KEYS: &[&str] = &[
    "enabled",
    "file_too_long",
    "function_too_long",
    "type_too_long",
    "too_many_parameters",
    "nesting_too_deep",
    "cyclomatic_complexity",
    "lazy_ignore",
    "magic_number",
    "law_of_demeter",
];

/// The integer (threshold) option keys, merged and declared like
/// [`BOOL_OPTION_KEYS`].
pub(crate) const INTEGER_OPTION_KEYS: &[&str] = &[
    "file_too_long_lines",
    "function_too_long_lines",
    "type_too_long_lines",
    "too_many_parameters_count",
    "nesting_too_deep_depth",
    "cyclomatic_complexity_max",
    "law_of_demeter_depth",
];

/// The array-valued option keys, merged and declared like [`BOOL_OPTION_KEYS`].
pub(crate) const ARRAY_OPTION_KEYS: &[&str] = &["magic_number_allow"];

/// Every option key the quality backend reads, in one slice for
/// `Engine::option_keys`.
pub(crate) static OPTION_KEYS: std::sync::LazyLock<Vec<&'static str>> = std::sync::LazyLock::new(|| {
    BOOL_OPTION_KEYS
        .iter()
        .chain(INTEGER_OPTION_KEYS)
        .chain(ARRAY_OPTION_KEYS)
        .copied()
        .collect()
});

/// The fully resolved quality-engine configuration for one file.
#[derive(Debug, Clone)]
pub struct Settings {
    /// Master switch; `false` disables every rule below regardless of their
    /// own toggles.
    pub enabled: bool,
    pub file_too_long: RuleSetting,
    pub function_too_long: RuleSetting,
    pub type_too_long: RuleSetting,
    pub too_many_parameters: RuleSetting,
    pub nesting_too_deep: RuleSetting,
    pub cyclomatic_complexity: RuleSetting,
    /// `lazy-ignore` has no numeric threshold.
    pub lazy_ignore: bool,
    /// Opt-in: off unless `[lint.quality] magic_number = true`.
    pub magic_number: bool,
    /// Numbers that never trigger `magic-number`, regardless of context.
    pub magic_number_allow: Vec<i64>,
    /// Opt-in: off unless `[lint.quality] law_of_demeter = true`.
    pub law_of_demeter: bool,
    pub law_of_demeter_depth: i64,
}

/// A rule with an `on`/`off` toggle and an integer threshold.
#[derive(Debug, Clone, Copy)]
pub struct RuleSetting {
    pub enabled: bool,
    pub threshold: i64,
}

impl Settings {
    /// Resolve settings from the merged `[lint.quality]` / per-language
    /// options table. Every key is optional; absent keys take the ADR 0027
    /// default shown in the module-level constants.
    pub fn from_config(cfg: &EngineConfig) -> Settings {
        let options = &cfg.options;
        Settings {
            enabled: flag(options, "enabled", true),
            file_too_long: rule(
                options,
                "file_too_long",
                "file_too_long_lines",
                true,
                DEFAULT_FILE_TOO_LONG,
            ),
            function_too_long: rule(
                options,
                "function_too_long",
                "function_too_long_lines",
                true,
                DEFAULT_FUNCTION_TOO_LONG,
            ),
            type_too_long: rule(
                options,
                "type_too_long",
                "type_too_long_lines",
                true,
                DEFAULT_TYPE_TOO_LONG,
            ),
            too_many_parameters: rule(
                options,
                "too_many_parameters",
                "too_many_parameters_count",
                true,
                DEFAULT_TOO_MANY_PARAMETERS,
            ),
            nesting_too_deep: rule(
                options,
                "nesting_too_deep",
                "nesting_too_deep_depth",
                true,
                DEFAULT_NESTING_TOO_DEEP,
            ),
            cyclomatic_complexity: rule(
                options,
                "cyclomatic_complexity",
                "cyclomatic_complexity_max",
                true,
                DEFAULT_CYCLOMATIC_COMPLEXITY,
            ),
            lazy_ignore: flag(options, "lazy_ignore", true),
            magic_number: flag(options, "magic_number", false),
            magic_number_allow: options
                .get("magic_number_allow")
                .and_then(toml::Value::as_array)
                .map(|values| values.iter().filter_map(toml::Value::as_integer).collect())
                .unwrap_or_else(|| DEFAULT_MAGIC_NUMBER_ALLOW.to_vec()),
            law_of_demeter: flag(options, "law_of_demeter", false),
            law_of_demeter_depth: integer(options, "law_of_demeter_depth", DEFAULT_LAW_OF_DEMETER),
        }
    }
}

fn flag(options: &toml::Table, key: &str, default: bool) -> bool {
    options.get(key).and_then(toml::Value::as_bool).unwrap_or(default)
}

fn integer(options: &toml::Table, key: &str, default: i64) -> i64 {
    options.get(key).and_then(toml::Value::as_integer).unwrap_or(default)
}

fn rule(
    options: &toml::Table,
    enabled_key: &str,
    threshold_key: &str,
    default_enabled: bool,
    default_threshold: i64,
) -> RuleSetting {
    RuleSetting {
        enabled: flag(options, enabled_key, default_enabled),
        threshold: integer(options, threshold_key, default_threshold),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{EngineConfig, GlobalDefaults};

    fn cfg(options: toml::Table) -> EngineConfig {
        EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 4,
            options,
        }
    }

    #[test]
    fn defaults_match_the_adr_table() {
        let settings = Settings::from_config(&cfg(toml::Table::new()));
        assert!(settings.enabled);
        assert!(settings.file_too_long.enabled);
        assert_eq!(settings.file_too_long.threshold, 1000);
        assert!(settings.function_too_long.enabled);
        assert_eq!(settings.function_too_long.threshold, 80);
        assert!(settings.type_too_long.enabled);
        assert_eq!(settings.type_too_long.threshold, 300);
        assert!(settings.too_many_parameters.enabled);
        assert_eq!(settings.too_many_parameters.threshold, 6);
        assert!(settings.nesting_too_deep.enabled);
        assert_eq!(settings.nesting_too_deep.threshold, 4);
        assert!(settings.cyclomatic_complexity.enabled);
        assert_eq!(settings.cyclomatic_complexity.threshold, 20);
        assert!(settings.lazy_ignore);
        assert!(!settings.magic_number, "magic-number is opt-in");
        assert_eq!(settings.magic_number_allow, vec![-1, 0, 1, 2, 10, 100]);
        assert!(!settings.law_of_demeter, "law-of-demeter is opt-in");
        assert_eq!(settings.law_of_demeter_depth, 3);
    }

    #[test]
    fn user_overrides_win_over_defaults() {
        let mut options = toml::Table::new();
        options.insert("function_too_long_lines".to_owned(), toml::Value::Integer(40));
        options.insert("magic_number".to_owned(), toml::Value::Boolean(true));
        let settings = Settings::from_config(&cfg(options));
        assert_eq!(settings.function_too_long.threshold, 40);
        assert!(settings.magic_number);
    }
}
