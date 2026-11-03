//! The PHP engine family: mago lint and format.

pub mod mago;

pub use mago::{LANGUAGES, MagoEngine};

use poly_engine_api::engine::Engine;
use poly_engine_api::language::Language;

/// The engines this family registers for `language`; empty when the family does not handle it.
pub fn engines_for(language: &Language) -> Vec<Box<dyn Engine>> {
    if LANGUAGES.contains(language) {
        vec![Box::new(MagoEngine::default())]
    } else {
        Vec::new()
    }
}
