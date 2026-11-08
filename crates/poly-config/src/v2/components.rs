//! `[components]` — which downloadable engine components a run may use and what
//! happens when one is unavailable.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use poly_component_index::{ComponentIndex, index};
use schemars::JsonSchema;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::EngineTable;

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

/// Set once by commands that must keep working on a config this check rejects (`poly config
/// show`, `poly components status`): they exist to inspect and repair that very state.
static SKIP_CHECK: AtomicBool = AtomicBool::new(false);

/// Warnings already printed this run; the same inherited table is seen by every nested config.
static WARNED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// Stop the load-time `[components]` check from failing or warning for the rest of the process.
pub fn skip_components_check() {
    SKIP_CHECK.store(true, Ordering::Relaxed);
}

/// Print `message` to stderr as a warning, once per run.
fn warn_once(message: &str) {
    let first = WARNED.lock().map_or(true, |mut seen| seen.insert(message.to_owned()));
    if first {
        eprintln!("warning: {message}");
    }
}

/// Check a loaded config's `[components]` against the component index.
///
/// Errors (the load fails): a name in `enable` that is neither a component nor a preset, and a
/// name in both `enable` and `disable`. Warnings (printed once per run to stderr, still an error
/// in `poly config validate`): an unknown name in `disable`, and an engine configured under a
/// disabled component (`[engines.<id>]`, `[lint.<lang>.<id>]`, `[fmt.<lang>.<id>]` or a
/// cross-cutting `[lint.<id>]`) unless that table, or its language table, sets `enabled = false`.
/// The warnings are not errors because an `extends` base or a parent monorepo config may
/// configure an engine a child config or `poly.local.toml` disables; that has always worked.
///
/// # Errors
///
/// The first error-class problem, as a message naming the offending key.
pub(crate) fn validate_components(
    components: &ComponentsConfig,
    lint: &toml::Table,
    fmt: &toml::Table,
    engines: &BTreeMap<String, EngineTable>,
) -> Result<(), String> {
    if SKIP_CHECK.load(Ordering::Relaxed) {
        return Ok(());
    }
    let (error, warnings) = check_components(components, lint, fmt, engines);
    if let Some(message) = error {
        return Err(message);
    }
    for message in &warnings {
        warn_once(message);
    }
    Ok(())
}

pub(crate) fn check_components(
    components: &ComponentsConfig,
    lint: &toml::Table,
    fmt: &toml::Table,
    engines: &BTreeMap<String, EngineTable>,
) -> (Option<String>, Vec<String>) {
    let index = index();
    let enabled: &[String] = match &components.enable {
        Enable::Auto => &[],
        Enable::List(list) => list,
    };
    let unknown = |key: &str, name: &str| {
        format!(
            "`[components] {key}` names `{name}`, which is neither a component nor a preset (known: {})",
            known_names(index)
        )
    };
    if let Some(name) = enabled.iter().find(|name| index.expand_preset(name).is_none()) {
        return (Some(unknown("enable", name)), Vec::new());
    }
    if let Some(name) = components.disable.iter().find(|name| enabled.contains(name)) {
        let message = format!("`{name}` is in both `[components] enable` and `[components] disable`");
        return (Some(message), Vec::new());
    }
    let mut warnings: Vec<String> = components
        .disable
        .iter()
        .filter(|name| index.expand_preset(name).is_none())
        .map(|name| unknown("disable", name))
        .collect();
    let disabled: BTreeSet<&str> = components
        .disable
        .iter()
        .filter_map(|name| index.expand_preset(name))
        .flatten()
        .map(|component| component.name.as_str())
        .collect();
    if disabled.is_empty() {
        return (None, warnings);
    }
    let configured = engines
        .iter()
        .filter(|(_, table)| table.enabled != Some(false))
        .map(|(id, _)| (format!("[engines.{id}]"), id.as_str()))
        .chain(v1_engine_tables(index, lint, "lint"))
        .chain(v1_engine_tables(index, fmt, "fmt"));
    for (table, id) in configured {
        if let Some(component) = index
            .component_for_engine(id)
            .filter(|c| disabled.contains(c.name.as_str()))
        {
            warnings.push(format!(
                "{table} configures engine `{id}`, but its component `{}` is disabled by `[components] disable`; \
                 the table is ignored (set `enabled = false` in it to silence this)",
                component.name
            ));
        }
    }
    (None, warnings)
}

fn known_names(index: &ComponentIndex) -> String {
    let mut names: Vec<&str> = index.components.iter().map(|c| c.name.as_str()).collect();
    names.extend(index.presets.keys().map(String::as_str));
    names.join(", ")
}

/// The engine tables of a v1 `[lint]` / `[fmt]` table that are not switched off: the
/// `[<phase>.<lang>.<engine>]` tables, and `[<phase>.<engine>]` for a cross-cutting engine (one
/// serving no particular language).
fn switched_on(table: &toml::Table) -> bool {
    table.get("enabled").and_then(toml::Value::as_bool) != Some(false)
}

fn v1_engine_tables<'a>(
    index: &'a ComponentIndex,
    phase: &'a toml::Table,
    name: &'a str,
) -> impl Iterator<Item = (String, &'a str)> + 'a {
    phase
        .iter()
        .filter_map(|(key, value)| Some((key, value.as_table()?)))
        .flat_map(move |(key, table)| {
            let cross_cutting = index
                .engine_descs(key)
                .next()
                .is_some_and(|desc| desc.languages.is_empty());
            let top = (cross_cutting && switched_on(table)).then(|| (format!("[{name}.{key}]"), key.as_str()));
            let nested = table
                .iter()
                .filter_map(|(engine, value)| Some((engine, value.as_table()?)))
                .filter(|(_, inner)| switched_on(table) && switched_on(inner))
                .map(move |(engine, _)| (format!("[{name}.{key}.{engine}]"), engine.as_str()));
            top.into_iter().chain(nested)
        })
}
