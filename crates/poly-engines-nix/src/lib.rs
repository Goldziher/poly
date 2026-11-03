//! The Nix engine family: the alejandra formatter.

pub mod nixfmt;

pub use nixfmt::{LANGUAGES, NixFmtEngine};

use poly_engine_api::engine::Engine;
use poly_engine_api::language::Language;

/// The engines this family registers for `language`; empty when the family does not handle it.
pub fn engines_for(language: &Language) -> Vec<Box<dyn Engine>> {
    if LANGUAGES.contains(language) {
        vec![Box::new(NixFmtEngine)]
    } else {
        Vec::new()
    }
}
