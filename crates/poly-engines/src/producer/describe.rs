//! Building the `describe()` payload from the hosted engines.

use poly_component_protocol::{Describe, PROTOCOL_VERSION, WireOptionTable};
use poly_engine_api::desc::engine_desc;
use poly_engine_api::engine::{Engine, OptionTable};

/// The option tables an engine declares keys for, with their wire names.
pub(crate) const TABLES: [(OptionTable, WireOptionTable); 3] = [
    (OptionTable::Lint, WireOptionTable::Lint),
    (OptionTable::Format, WireOptionTable::Format),
    (OptionTable::CrossCuttingLint, WireOptionTable::CrossCuttingLint),
];

/// Engines are listed in the order the compiled component index records them, by name and then
/// languages, because the host compares a component's description with its index entry as is.
pub(crate) fn describe(component_version: &str, engines: &[Box<dyn Engine>]) -> Describe {
    let mut engines: Vec<_> = engines.iter().map(|engine| engine_desc(engine.as_ref())).collect();
    engines.sort_by(|a, b| (&a.name, &a.languages).cmp(&(&b.name, &b.languages)));
    Describe {
        protocol: PROTOCOL_VERSION,
        component_version: component_version.to_owned(),
        engines,
    }
}
