//! `[hooks.builtin]` — poly's first-class in-process hooks (`polylint`,
//! `polyfmt`, `poly commit`).
//!
//! Each builtin is either a bare boolean (`polylint = true`, enable with the
//! default stages) or a table (`polyfmt = { stages = ["pre-commit"] }`); a
//! table without an explicit `enabled` key is treated as enabled.

use serde::{Deserialize, Deserializer};

/// `[hooks.builtin]` — poly's first-class in-process hooks.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct BuiltinHooks {
    /// The `polylint` linter hook.
    pub polylint: BuiltinHook,
    /// The `polyfmt` formatter hook.
    pub polyfmt: BuiltinHook,
    /// The `poly commit` message-lint hook.
    pub commit: BuiltinHook,
}

/// One builtin hook. Accepts either a bare boolean (`polylint = true`) or a
/// table (`polyfmt = { stages = ["pre-commit"] }`); a table without an explicit
/// `enabled` key is treated as enabled.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuiltinHook {
    /// Whether this builtin hook is active.
    pub enabled: bool,
    /// Stages this hook runs in; empty means inherit [`super::HooksConfig::stages`].
    pub stages: Vec<String>,
}

/// On-disk form of a builtin hook: bare toggle or a table.
#[derive(Deserialize)]
#[serde(untagged)]
enum BuiltinHookRepr {
    Toggle(bool),
    Table(BuiltinHookTable),
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct BuiltinHookTable {
    enabled: Option<bool>,
    stages: Vec<String>,
}

impl<'de> Deserialize<'de> for BuiltinHook {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match BuiltinHookRepr::deserialize(deserializer)? {
            BuiltinHookRepr::Toggle(enabled) => Ok(BuiltinHook {
                enabled,
                stages: Vec::new(),
            }),
            // Presence of a table implies the hook is enabled unless it says otherwise.
            BuiltinHookRepr::Table(table) => Ok(BuiltinHook {
                enabled: table.enabled.unwrap_or(true),
                stages: table.stages,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_toggle_enables_with_no_stages() {
        let hooks: BuiltinHooks = toml::from_str("polylint = true").unwrap();
        assert!(hooks.polylint.enabled);
        assert!(hooks.polylint.stages.is_empty());
        assert!(!hooks.polyfmt.enabled);
    }

    #[test]
    fn table_without_enabled_is_enabled() {
        let hooks: BuiltinHooks =
            toml::from_str(r#"polyfmt = { stages = ["pre-commit"] }"#).unwrap();
        assert!(hooks.polyfmt.enabled);
        assert_eq!(hooks.polyfmt.stages, vec!["pre-commit".to_string()]);
    }

    #[test]
    fn table_with_explicit_disable() {
        let hooks: BuiltinHooks = toml::from_str("commit = { enabled = false }").unwrap();
        assert!(!hooks.commit.enabled);
    }
}
