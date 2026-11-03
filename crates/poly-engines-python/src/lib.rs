//! The Python engine family: ruff lint and format, and ty type checking.

pub mod ruff;
#[cfg(feature = "ty")]
pub mod ty;

/// Re-exported so the host's tests can read ruff's rule registry without a second dependency edge.
pub use ruff_linter;

pub use ruff::{LANGUAGES, RuffEngine};
#[cfg(feature = "ty")]
pub use ty::TyEngine;

use poly_engine_api::engine::Engine;
use poly_engine_api::language::Language;

/// The engines this family registers for `language`; empty when the family does not handle it.
pub fn engines_for(language: &Language) -> Vec<Box<dyn Engine>> {
    if !LANGUAGES.contains(language) {
        return Vec::new();
    }
    #[cfg(feature = "ty")]
    let engines: Vec<Box<dyn Engine>> = vec![Box::new(RuffEngine), Box::new(TyEngine)];
    #[cfg(not(feature = "ty"))]
    let engines: Vec<Box<dyn Engine>> = vec![Box::new(RuffEngine)];
    engines
}
