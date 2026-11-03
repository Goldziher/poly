//! The Python engine family: ruff lint and format.

pub mod ruff;

/// Re-exported so the host's tests can read ruff's rule registry without a second dependency edge.
pub use ruff_linter;

pub use ruff::{LANGUAGES, RuffEngine};

use poly_engine_api::engine::Engine;
use poly_engine_api::language::Language;

/// The engines this family registers for `language`; empty when the family does not handle it.
pub fn engines_for(language: &Language) -> Vec<Box<dyn Engine>> {
    if LANGUAGES.contains(language) {
        vec![Box::new(RuffEngine)]
    } else {
        Vec::new()
    }
}
