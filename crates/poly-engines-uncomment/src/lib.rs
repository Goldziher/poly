//! The uncomment engine family: the opt-in cross-cutting comment-removal lint.

pub mod uncomment;

pub use uncomment::UncommentEngine;

use poly_engine_api::engine::Engine;
use poly_engine_api::language::Language;

/// The engines this family registers for `language`: uncomment is cross-cutting, so every language.
pub fn engines_for(_language: &Language) -> Vec<Box<dyn Engine>> {
    vec![Box::new(UncommentEngine)]
}
