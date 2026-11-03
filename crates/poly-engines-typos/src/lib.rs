//! The typos engine family: the cross-cutting spell-checker.

pub mod typos;

pub use typos::TyposEngine;

use poly_engine_api::engine::Engine;
use poly_engine_api::language::Language;

/// The engines this family registers for `language`: typos is cross-cutting, so every language.
pub fn engines_for(_language: &Language) -> Vec<Box<dyn Engine>> {
    vec![Box::new(TyposEngine)]
}
