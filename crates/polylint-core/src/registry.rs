//! Backend registry: maps a [`Language`] to the ordered list of engines that
//! handle it. Native backends are wired here as they land; the
//! [`TreeSitterEngine`] generic tier serves any language no native backend has
//! claimed (structural reindent for brace grammars, whitespace normalization
//! otherwise).

use crate::engine::Engine;
use crate::engines::graphql::GraphQlEngine;
use crate::engines::mago::MagoEngine;
use crate::engines::malva::MalvaEngine;
use crate::engines::markup_fmt::MarkupFmtEngine;
use crate::engines::nixfmt::NixFmtEngine;
use crate::engines::oxc::OxcEngine;
use crate::engines::ruff::RuffEngine;
use crate::engines::rumdl::RumdlEngine;
use crate::engines::sqruff::SqruffEngine;
use crate::engines::taplo::TaploEngine;
use crate::engines::treesitter::TreeSitterEngine;
use crate::engines::typos::TyposEngine;
use crate::engines::yaml::YamlEngine;
use crate::language::Language;

/// Engines applicable to a language, in priority order (formatters run in sequence).
pub fn engines_for(lang: &Language) -> Vec<Box<dyn Engine>> {
    let mut engines: Vec<Box<dyn Engine>> = match lang {
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
        Language::Yaml => vec![Box::new(YamlEngine)],
        Language::Css | Language::Scss | Language::Less => vec![Box::new(MalvaEngine)],
        Language::Nix => vec![Box::new(NixFmtEngine)],
        Language::GraphQl => vec![Box::new(GraphQlEngine)],
        Language::Html | Language::Vue | Language::Svelte => vec![Box::new(MarkupFmtEngine)],
        Language::Php => vec![Box::new(MagoEngine)],
        // As other native backends land they are matched here, falling through
        // to the tree-sitter generic tier for everything else.
        _ => vec![Box::new(TreeSitterEngine)],
    };
    // typos is cross-cutting: every file is spell-checked in addition to its
    // language-specific lint/format engines.
    engines.push(Box::new(TyposEngine));
    engines
}
