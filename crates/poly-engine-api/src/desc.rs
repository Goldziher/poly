//! The wire description of an engine: the one mapping from an [`Engine`] to a [`WireEngineDesc`].
//!
//! Component producers answer `describe()` with it and the compiled component index generator
//! records it, so both read the same fields the same way.

use poly_component_protocol::{
    WireCapabilities, WireEngineDesc, WireOptionKey, WireOptionKeys, WireOptionTable, hooks,
};

use crate::engine::{Engine, OptionKeys, OptionTable};

/// Every optional per-engine request the host may send ([`hooks`]).
///
/// An unset bit lets the host skip a call, so claiming all of them is always correct; it only
/// forgoes the saving. The trait cannot say which defaults an engine overrides, so a producer that
/// knows better narrows this per engine.
pub const ALL_HOOKS: u32 = hooks::RUN_CONTEXT
    | hooks::FILE_CONTEXT
    | hooks::PROVIDES_LANGUAGE
    | hooks::SKIP_REASON
    | hooks::LINT_INPUT_SALT
    | hooks::FORMAT_INPUT_SALT
    | hooks::LINT_CACHE_SALT
    | hooks::BATCH_SUPPORT;

/// Describe one option table of `engine`.
fn option_keys_desc(keys: &OptionKeys, table: WireOptionTable) -> WireOptionKeys {
    WireOptionKeys {
        table,
        checked: keys.is_checked(),
        declared: keys
            .declared_keys()
            .iter()
            .map(|(name, option_type)| WireOptionKey {
                name: (*name).to_owned(),
                types: option_type.bits(),
            })
            .collect(),
        rule_selection: keys.has_rule_selection(),
        has_derived: keys.derived_probe().is_some(),
        note: keys.note().map(str::to_owned),
    }
}

/// The wire description of `engine`, covering every [`WireEngineDesc`] field.
#[must_use]
pub fn engine_desc(engine: &dyn Engine) -> WireEngineDesc {
    let capabilities = engine.capabilities();
    let host_hook = serde_json::to_value(engine.host_hook())
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "none".to_owned());
    let tables = [
        (OptionTable::Lint, WireOptionTable::Lint),
        (OptionTable::Format, WireOptionTable::Format),
        (OptionTable::CrossCuttingLint, WireOptionTable::CrossCuttingLint),
    ];
    WireEngineDesc {
        name: engine.name().to_owned(),
        languages: engine.languages().iter().map(|l| l.id().to_owned()).collect(),
        capabilities: WireCapabilities {
            lint: capabilities.lint,
            format: capabilities.format,
            fix: capabilities.fix,
            project: capabilities.project,
        },
        version: engine.version().to_owned(),
        option_keys: tables
            .into_iter()
            .map(|(table, wire)| option_keys_desc(&engine.option_keys(table), wire))
            .collect(),
        hooks: ALL_HOOKS,
        takes_over_tier_one: engine.takes_over_tier_one(),
        claimed_paths: engine.claimed_paths(),
        host_hook,
        supplements_tier_one: engine.supplements_tier_one(),
        self_manages_enabled: engine.self_manages_enabled(),
        supersedes_generic_formatter: engine.supersedes_generic_formatter(),
        batch_shard_floor: u32::try_from(engine.batch_shard_floor()).unwrap_or(u32::MAX),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Capabilities, Engine};
    use crate::language::Language;

    struct Probe;

    impl Engine for Probe {
        fn name(&self) -> &'static str {
            "probe"
        }
        fn languages(&self) -> &'static [Language] {
            &[Language::Python]
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities {
                lint: true,
                ..Capabilities::default()
            }
        }
        fn version(&self) -> &str {
            "1.2.3"
        }
    }

    #[test]
    fn describes_every_field_of_a_default_engine() {
        let desc = engine_desc(&Probe);
        assert_eq!(desc.name, "probe");
        assert_eq!(desc.languages, ["python"]);
        assert!(desc.capabilities.lint && !desc.capabilities.format);
        assert_eq!(desc.version, "1.2.3");
        assert_eq!(desc.option_keys.len(), 3);
        assert!(
            desc.option_keys
                .iter()
                .all(|keys| !keys.checked && keys.declared.is_empty())
        );
        assert_eq!(desc.hooks, ALL_HOOKS);
        assert_eq!(desc.host_hook, "none");
        assert_eq!(desc.batch_shard_floor, 2);
        assert!(desc.claimed_paths.is_empty() && !desc.takes_over_tier_one);
    }
}
