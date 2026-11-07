//! The compiled index of poly's engine components.
//!
//! The index is data: `data/index.json`, generated from the registered engines by the drift test
//! in `poly-core` (`UPDATE_INDEX=1 cargo test -p poly-core --test component_index`) and embedded
//! here at compile time. It lets the core answer, without loading any component, which component
//! hosts an engine or serves a language, what an engine's static option keys are, and what a
//! preset such as `all` or `js` expands to. This crate depends on no engine crate.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use poly_component_protocol::{WireEngineDesc, WireOptionKeys, WireOptionTable};
use serde::{Deserialize, Serialize};

/// The committed index, as embedded.
pub const INDEX_JSON: &str = include_str!("../data/index.json");

/// What a component contains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ComponentKind {
    /// Hosts engines.
    #[default]
    Engines,
    /// A front end (such as the MCP server) that hosts no engines.
    Frontend,
}

/// One downloadable component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Component {
    /// Component (package) name.
    pub name: String,
    /// What the component contains.
    #[serde(default)]
    pub kind: ComponentKind,
    /// Not part of the `all` preset; installed only when asked for.
    #[serde(default)]
    pub opt_in: bool,
    /// The `poly-core` cargo features (engine families) the component is built from.
    #[serde(default)]
    pub features: Vec<String>,
    /// The engines it hosts, in full wire form.
    #[serde(default)]
    pub engines: Vec<WireEngineDesc>,
    /// Lock sha of the released artifact; `None` until release CI fills it.
    #[serde(default)]
    pub lock_sha: Option<String>,
}

/// The whole index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ComponentIndex {
    /// Schema version of the file.
    #[serde(default)]
    pub schema: u32,
    /// Every component.
    #[serde(default)]
    pub components: Vec<Component>,
    /// Preset name to the component names it expands to (`all`, language aliases such as `js`).
    #[serde(default)]
    pub presets: BTreeMap<String, Vec<String>>,
}

/// The index could not be read.
#[derive(Debug, thiserror::Error)]
#[error("invalid component index: {0}")]
pub struct IndexError(#[from] serde_json::Error);

impl ComponentIndex {
    /// Parse an index from JSON.
    ///
    /// # Errors
    /// When `json` is not a valid index document.
    pub fn parse(json: &str) -> Result<Self, IndexError> {
        Ok(serde_json::from_str(json)?)
    }

    /// The component named `name`.
    #[must_use]
    pub fn component(&self, name: &str) -> Option<&Component> {
        self.components.iter().find(|c| c.name == name)
    }

    /// The engines `component` hosts; empty for an unknown component or a front end.
    #[must_use]
    pub fn engines(&self, component: &str) -> &[WireEngineDesc] {
        self.component(component).map_or(&[], |c| c.engines.as_slice())
    }

    /// The component hosting the engine named `engine`.
    #[must_use]
    pub fn component_for_engine(&self, engine: &str) -> Option<&Component> {
        self.components
            .iter()
            .find(|c| c.engines.iter().any(|e| e.name == engine))
    }

    /// Every description of the engine named `engine`: one engine may be described once per
    /// language family it serves (`biome` lints CSS and GraphQL).
    pub fn engine_descs<'a, 'n>(&'a self, engine: &'n str) -> impl Iterator<Item = &'a WireEngineDesc> + use<'a, 'n> {
        self.components
            .iter()
            .flat_map(|c| c.engines.iter())
            .filter(move |e| e.name == engine)
    }

    /// Whether some engine declares that `key` set to `value` selects it in config table `table`.
    #[must_use]
    pub fn claims_selection(&self, table: &str, key: &str, value: &str) -> bool {
        self.components
            .iter()
            .flat_map(|c| c.engines.iter())
            .filter_map(|e| e.selection.as_ref())
            .any(|s| s.table == table && s.key == key && s.value == value)
    }

    /// Components hosting an engine registered for the language id `language`, in index order.
    /// Cross-cutting engines (no languages) are not matched; see
    /// [`ComponentIndex::cross_cutting_components`].
    #[must_use]
    pub fn components_for_language(&self, language: &str) -> Vec<&Component> {
        self.components
            .iter()
            .filter(|c| c.engines.iter().any(|e| e.languages.iter().any(|l| l == language)))
            .collect()
    }

    /// Components hosting an engine that applies to every language.
    #[must_use]
    pub fn cross_cutting_components(&self) -> Vec<&Component> {
        self.components
            .iter()
            .filter(|c| c.engines.iter().any(|e| e.languages.is_empty()))
            .collect()
    }

    /// The components a preset or a bare component name selects, in index order, or `None` when
    /// `name` is neither. Components a preset names but the index lacks are dropped.
    #[must_use]
    pub fn expand_preset(&self, name: &str) -> Option<Vec<&Component>> {
        let names: Vec<&str> = match self.presets.get(name) {
            Some(names) => names.iter().map(String::as_str).collect(),
            None => vec![self.component(name)?.name.as_str()],
        };
        Some(
            self.components
                .iter()
                .filter(|c| names.contains(&c.name.as_str()))
                .collect(),
        )
    }

    /// The static option keys `engine` declares for `table`, for config validation without
    /// loading its component. With `language`, picks the description serving that language
    /// (an engine described once per language family); without, the first description.
    #[must_use]
    pub fn option_keys(&self, engine: &str, language: Option<&str>, table: WireOptionTable) -> Option<&WireOptionKeys> {
        let mut descs = self.engine_descs(engine);
        let desc = match language {
            Some(language) => descs.find(|e| e.languages.is_empty() || e.languages.iter().any(|l| l == language)),
            None => descs.next(),
        }?;
        desc.option_keys.iter().find(|keys| keys.table == table)
    }
}

/// The embedded index, parsed once.
///
/// # Panics
/// When the committed `data/index.json` is malformed, which the unit test of this crate rules out.
#[must_use]
pub fn index() -> &'static ComponentIndex {
    static INDEX: OnceLock<ComponentIndex> = OnceLock::new();
    INDEX.get_or_init(|| ComponentIndex::parse(INDEX_JSON).expect("data/index.json is a valid component index"))
}

#[cfg(test)]
mod tests;
