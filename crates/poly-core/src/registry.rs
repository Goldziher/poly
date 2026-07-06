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
        // MalvaEngine holds the format slot for CSS/SCSS/Less.
        // BiomeCssEngine adds rule-based lint for CSS and SCSS; Less has no
        // biome parser support so it falls through to MalvaEngine only.
        Language::Less => vec![Box::new(MalvaEngine)],
        Language::Css | Language::Scss => vec![Box::new(MalvaEngine), Box::new(BiomeCssEngine)],
        Language::Nix => vec![Box::new(NixFmtEngine)],
        Language::Ruby => vec![Box::new(RubyfmtEngine)],
        // GraphQlEngine holds the format slot and emits parse-error lint.
        // BiomeGraphqlEngine adds rule-based lint (correctness/suspicious groups).
        // The runner filters by capability so both coexist without conflict.
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
        // Native toolchain backends. Each `NativeToolEngine` takes the registry
        // slot that `TreeSitterEngine` would otherwise occupy; it delegates
        // internally to `TreeSitterEngine` when the tool is disabled or absent.
        // This guarantees exactly one formatter runs per file (no double-format).
        //
        // The canonical formatters `rustfmt` (Rust) and `gofmt` (Go) are
        // DEFAULT-ON when detected on PATH: present + enabled → the native tool
        // wins over the tree-sitter generic tier; absent → tier-2 fallback with a
        // once-per-language info notice. `zig fmt` stays opt-in (off by default).
        Language::Go => vec![Box::new(NativeToolEngine::for_language(Language::Go))],
        Language::Rust => vec![Box::new(NativeToolEngine::for_language(Language::Rust))],
        Language::Zig => vec![Box::new(NativeToolEngine::for_language(Language::Zig))],
        // Opt-in native backends for JVM, R, Apple, Dart, and Gleam toolchains.
        // Each delegates to TreeSitterEngine when disabled or absent, so the
        // language is never left without formatting (see NativeToolEngine docs).
        Language::Java => vec![Box::new(NativeToolEngine::for_language(Language::Java))],
        Language::Kotlin => vec![Box::new(NativeToolEngine::for_language(Language::Kotlin))],
        Language::R => vec![Box::new(NativeToolEngine::for_language(Language::R))],
        Language::Swift => vec![Box::new(NativeToolEngine::for_language(Language::Swift))],
        Language::Dart => vec![Box::new(NativeToolEngine::for_language(Language::Dart))],
        Language::Gleam => vec![Box::new(NativeToolEngine::for_language(Language::Gleam))],
        // Shell: two opt-in native tools registered separately.
        // `shell_format()` (shfmt) holds the format slot; `shell_lint()`
        // (shellcheck) holds the lint slot. Both delegate to TreeSitterEngine
        // when disabled or absent so the language is never left without
        // formatting or linting.
        Language::Shell => vec![
            Box::new(NativeToolEngine::shell_format()),
            Box::new(NativeToolEngine::shell_lint()),
        ],
        // As other native backends land they are matched here, falling through
        // to the tree-sitter generic tier for everything else.
        _ => vec![Box::new(TreeSitterEngine)],
    };
    // typos is cross-cutting: every file is spell-checked in addition to its
    // language-specific lint/format engines.
    engines.push(Box::new(TyposEngine));
    // astgrep is cross-cutting: user-authored custom rules run on every file
    // whose language has matching rules in the configured [rules] dirs.
    engines.push(Box::new(AstGrepEngine));
    engines
}
