//! Backend registry: maps a [`Language`] to the ordered list of engines that
//! handle it. Native backends are wired here as they land; the
//! [`TreeSitterEngine`] generic tier serves any language no native backend has
//! claimed (structural reindent for brace grammars, whitespace normalization
//! otherwise).

use crate::engine::Engine;
use crate::engines::astgrep::AstGrepEngine;
use crate::engines::biome_css::BiomeCssEngine;
use crate::engines::biome_graphql::BiomeGraphqlEngine;
use crate::engines::dockerfile::DockerfileEngine;
use crate::engines::graphql::GraphQlEngine;
use crate::engines::hcl::HclEngine;
use crate::engines::mago::MagoEngine;
use crate::engines::malva::MalvaEngine;
use crate::engines::markup_fmt::MarkupFmtEngine;
use crate::engines::native_tool::NativeToolEngine;
use crate::engines::nixfmt::NixFmtEngine;
use crate::engines::oxc::OxcEngine;
use crate::engines::rubyfmt::RubyfmtEngine;
use crate::engines::ruff::RuffEngine;
use crate::engines::rumdl::RumdlEngine;
use crate::engines::sqruff::SqruffEngine;
use crate::engines::taplo::TaploEngine;
use crate::engines::treesitter::TreeSitterEngine;
use crate::engines::typos::TyposEngine;
use crate::engines::uncomment::UncommentEngine;
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
        Language::Less => vec![Box::new(MalvaEngine)],
        Language::Css | Language::Scss => vec![Box::new(MalvaEngine), Box::new(BiomeCssEngine)],
        Language::Nix => vec![Box::new(NixFmtEngine)],
        Language::Ruby => vec![Box::new(RubyfmtEngine)],
        Language::GraphQl => vec![Box::new(GraphQlEngine), Box::new(BiomeGraphqlEngine)],
        Language::Hcl => vec![Box::new(HclEngine)],
        Language::Html
        | Language::Vue
        | Language::Svelte
        | Language::Astro
        | Language::Angular
        | Language::Jinja
        | Language::Vento
        | Language::Mustache
        | Language::Xml => vec![Box::new(MarkupFmtEngine)],
        Language::Php => vec![Box::new(MagoEngine::default())],
        Language::Dockerfile => vec![Box::new(DockerfileEngine)],
        Language::Go => vec![Box::new(NativeToolEngine::for_language(Language::Go))],
        Language::Rust => vec![Box::new(NativeToolEngine::for_language(Language::Rust))],
        Language::Zig => vec![Box::new(NativeToolEngine::for_language(Language::Zig))],
        Language::Java => vec![Box::new(NativeToolEngine::for_language(Language::Java))],
        Language::Kotlin => vec![Box::new(NativeToolEngine::for_language(Language::Kotlin))],
        Language::R => vec![Box::new(NativeToolEngine::for_language(Language::R))],
        Language::Swift => vec![Box::new(NativeToolEngine::for_language(Language::Swift))],
        Language::Dart => vec![Box::new(NativeToolEngine::for_language(Language::Dart))],
        Language::Gleam => vec![Box::new(NativeToolEngine::for_language(Language::Gleam))],
        Language::Shell => vec![
            Box::new(NativeToolEngine::shell_format()),
            Box::new(NativeToolEngine::shell_lint()),
        ],
        _ => vec![Box::new(TreeSitterEngine)],
    };
    engines.push(Box::new(TyposEngine));
    engines.push(Box::new(AstGrepEngine));
    engines.push(Box::new(UncommentEngine));
    engines
}
