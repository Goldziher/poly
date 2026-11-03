//! `[components]` — which downloadable engine components a run may use and what
//! happens when one is unavailable.

use schemars::JsonSchema;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Which components `poly` makes available: everything the discovered files
/// need (`"auto"`), or an explicit list of component names and presets.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Enable {
    /// Derive the needed components from the discovered languages and the
    /// explicitly configured `[engines.*]` tables.
    #[default]
    Auto,
    /// Exactly these components or presets (`["python", "web"]`).
    List(Vec<String>),
}

/// The two on-disk spellings of [`Enable`].
#[derive(Deserialize)]
#[serde(untagged)]
enum EnableRepr {
    Word(String),
    List(Vec<String>),
}

impl<'de> Deserialize<'de> for Enable {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match EnableRepr::deserialize(deserializer)? {
            EnableRepr::Word(word) if word == "auto" => Ok(Enable::Auto),
            EnableRepr::Word(word) => Err(D::Error::custom(format!(
                "invalid `components.enable` value `{word}`: expected \"auto\" or an array of component names"
            ))),
            EnableRepr::List(list) => Ok(Enable::List(list)),
        }
    }
}

impl Serialize for Enable {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Enable::Auto => serializer.serialize_str("auto"),
            Enable::List(list) => list.serialize(serializer),
        }
    }
}

impl JsonSchema for Enable {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ComponentsEnable".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "`\"auto\"` (derive from the discovered files) or an explicit list of component \
                            names and presets.",
            "oneOf": [
                { "const": "auto" },
                { "type": "array", "items": { "type": "string" } }
            ]
        })
    }
}

/// Whether `poly` may download a missing component on its own.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ComponentInstallPolicy {
    /// Download and verify a missing component when a run needs it.
    #[default]
    Auto,
    /// Never download; a missing component is handled by `on_missing`.
    Never,
}

/// What a run does when a needed component is not available.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OnMissing {
    /// Fail the run (exit 2).
    #[default]
    Fail,
    /// Skip the files the component would have handled and report the skip.
    Skip,
}

/// `[components]` — selection, install and missing-component policy.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ComponentsConfig {
    /// `"auto"` (default) or an explicit list of component names and presets.
    pub enable: Enable,
    /// Components never to use, even when `enable = "auto"` would select them.
    pub disable: Vec<String>,
    /// Whether poly may download missing components. Defaults to `"auto"`.
    pub install: ComponentInstallPolicy,
    /// What to do when a needed component is unavailable. Defaults to `"fail"`.
    pub on_missing: OnMissing,
}
