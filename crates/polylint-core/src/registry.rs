//! Backend registry: maps a [`Language`] to the ordered list of engines that
//! handle it. Native backends are wired here as they land (M2+); the reference
//! [`WhitespaceEngine`] currently serves every language and also stands in for
//! the tree-sitter generic tier until M5.

use crate::engine::Engine;
use crate::engines::taplo::TaploEngine;
use crate::engines::whitespace::WhitespaceEngine;
use crate::language::Language;

/// Engines applicable to a language, in priority order (formatters run in sequence).
pub fn engines_for(lang: &Language) -> Vec<Box<dyn Engine>> {
    match lang {
        Language::Toml => vec![Box::new(TaploEngine::new())],
        // As other native backends land they will be matched here, e.g.:
        //   Language::Python => vec![Box::new(RuffEngine::new())],
        // falling through to the generic tier for everything else.
        _ => vec![Box::new(WhitespaceEngine)],
    }
}
