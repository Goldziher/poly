//! Backend registry: maps a [`Language`] to the ordered list of engines that
//! handle it. Native backends are wired here as they land (M2+); the reference
//! [`WhitespaceEngine`] serves any language not yet claimed by a native backend
//! and stands in for the tree-sitter generic tier until M5.

use crate::engine::Engine;
use crate::engines::oxc::OxcEngine;
use crate::engines::ruff::RuffEngine;
use crate::engines::rumdl::RumdlEngine;
use crate::engines::sqruff::SqruffEngine;
use crate::engines::taplo::TaploEngine;
use crate::engines::whitespace::WhitespaceEngine;
use crate::language::Language;

/// Engines applicable to a language, in priority order (formatters run in sequence).
pub fn engines_for(lang: &Language) -> Vec<Box<dyn Engine>> {
    match lang {
        Language::JavaScript
        | Language::TypeScript
        | Language::Jsx
        | Language::Tsx
        | Language::Json
        | Language::Jsonc => vec![Box::new(OxcEngine)],
        Language::Toml => vec![Box::new(TaploEngine::new())],
        Language::Markdown => vec![Box::new(RumdlEngine)],
        Language::Python => vec![Box::new(RuffEngine)],
        Language::Sql => vec![Box::new(SqruffEngine)],
        // As other native backends land they are matched here, falling through
        // to the generic tier for everything else.
        _ => vec![Box::new(WhitespaceEngine)],
    }
}
