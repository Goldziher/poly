//! Backend registry: maps a [`Language`] to the ordered list of engines that
//! handle it. Native backends are wired here as they land (M2+); the reference
//! [`WhitespaceEngine`] currently serves every language and also stands in for
//! the tree-sitter generic tier until M5.

use crate::engine::Engine;
use crate::engines::whitespace::WhitespaceEngine;
use crate::language::Language;

/// Engines applicable to a language, in priority order (formatters run in sequence).
pub fn engines_for(_lang: &Language) -> Vec<Box<dyn Engine>> {
    // As native backends land they are matched here, e.g.:
    //   Language::Toml => vec![Box::new(TaploEngine::new())],
    //   Language::Python => vec![Box::new(RuffEngine::new())],
    // falling through to the generic tier for everything else.
    vec![Box::new(WhitespaceEngine)]
}
