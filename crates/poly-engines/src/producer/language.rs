//! Language ids on the wire back to [`Language`].

use poly_engine_api::language::Language;

static NAMED: &[Language] = &[
    Language::Python,
    Language::JavaScript,
    Language::TypeScript,
    Language::Jsx,
    Language::Tsx,
    Language::Json,
    Language::Jsonc,
    Language::Yaml,
    Language::Toml,
    Language::Markdown,
    Language::Mdx,
    Language::Sql,
    Language::Css,
    Language::Scss,
    Language::Less,
    Language::Html,
    Language::Vue,
    Language::Svelte,
    Language::Astro,
    Language::Angular,
    Language::Jinja,
    Language::Vento,
    Language::Mustache,
    Language::Xml,
    Language::GraphQl,
    Language::Hcl,
    Language::Nix,
    Language::Shell,
    Language::Dockerfile,
    Language::Go,
    Language::Java,
    Language::Kotlin,
    Language::Ruby,
    Language::Php,
    Language::R,
    Language::Elixir,
    Language::C,
    Language::Cpp,
    Language::Rust,
    Language::Proto,
    Language::Zig,
    Language::Swift,
    Language::Dart,
    Language::Gleam,
    Language::CSharp,
    Language::Dotenv,
    Language::Ini,
];

/// The [`Language`] whose [`Language::id`] is `id`; an id no variant owns is [`Language::Other`].
pub(crate) fn language_from_id(id: &str) -> Language {
    NAMED
        .iter()
        .find(|language| language.id() == id)
        .cloned()
        .unwrap_or_else(|| Language::Other(id.to_owned()))
}
