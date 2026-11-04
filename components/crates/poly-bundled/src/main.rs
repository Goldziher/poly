//! The full `poly` binary: `poly-cli` with every engine family linked in.
//!
//! Families whose upstream crates are git pins live in this workspace, so the root `poly` core
//! cannot link them. This binary registers them as an engine provider before handing over to the
//! CLI, and tells the component planner they are linked so it never offers to download them.

use std::process::ExitCode;

use poly_core::components::register_linked_features;
use poly_core::{Engine, Language, register_engine_provider};

/// Component feature names served by [`engines_for`], as the committed component index spells them.
const LINKED_FEATURES: &[&str] = &["engine-biome", "engine-ruby"];

/// Every family's engines for `language`; a further family is one more `extend`.
fn engines_for(language: &Language) -> Vec<Box<dyn Engine>> {
    let mut engines = poly_engines_biome::engines_for(language);
    engines.extend(poly_engines_ruby::engines_for(language));
    engines
}

fn main() -> ExitCode {
    if let Err(err) = register_engine_provider(engines_for) {
        eprintln!("poly: cannot register the bundled engines: {err}");
        return ExitCode::from(2);
    }
    register_linked_features(LINKED_FEATURES);
    poly_cli::cli::main()
}
