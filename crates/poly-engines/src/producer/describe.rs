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

pub(crate) fn describe(component_version: &str, engines: &[Box<dyn Engine>]) -> Describe {
    Describe {
        protocol: PROTOCOL_VERSION,
        component_version: component_version.to_owned(),
        engines: engines.iter().map(|engine| engine_desc(engine.as_ref())).collect(),
    }
}
