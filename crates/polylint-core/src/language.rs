//! Language identification and per-language defaults.

use std::path::Path;

/// A source language / file format. Tier-1 languages have dedicated variants;
/// anything else identified by the tree-sitter language pack is [`Language::Other`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Language {
    /// Python.
    Python,
    /// JavaScript.
    JavaScript,
    /// TypeScript.
    TypeScript,
    /// JSX (React JavaScript).
    Jsx,
    /// TSX (React TypeScript).
    Tsx,
    /// JSON.
    Json,
    /// JSON with comments.
    Jsonc,
    /// YAML.
    Yaml,
    /// TOML.
    Toml,
    /// Markdown.
    Markdown,
    /// SQL.
    Sql,
    /// CSS.
    Css,
    /// SCSS.
    Scss,
    /// Less.
    Less,
    /// HTML.
    Html,
    /// Vue single-file component.
    Vue,
    /// Svelte component.
    Svelte,
    /// Astro component.
    Astro,
    /// Angular component template (matched via `*.component.html` filename convention).
    Angular,
    /// Jinja2 / Twig / Nunjucks template.
    Jinja,
    /// Vento template.
    Vento,
    /// Mustache / Handlebars template.
    Mustache,
    /// XML / SVG document.
    Xml,
    /// GraphQL.
    GraphQl,
    /// Nix.
    Nix,
    /// Shell / Bash.
    Shell,
    /// Dockerfile.
    Dockerfile,
    /// Go.
    Go,
    /// Java.
    Java,
    /// Kotlin.
    Kotlin,
    /// Ruby.
    Ruby,
    /// PHP.
    Php,
    /// R.
    R,
    /// Elixir.
    Elixir,
    /// C.
    C,
    /// C++.
    Cpp,
    /// Rust.
    Rust,
    /// Protocol Buffers.
    Proto,
    /// Any other language, identified by its tree-sitter-language-pack id.
    Other(String),
}

impl Language {
    /// Canonical lowercase id used in config tables and the tree-sitter pack.
    pub fn id(&self) -> &str {
        match self {
            Language::Python => "python",
            Language::JavaScript => "javascript",
            Language::TypeScript => "typescript",
            Language::Jsx => "jsx",
            Language::Tsx => "tsx",
            Language::Json => "json",
            Language::Jsonc => "jsonc",
            Language::Yaml => "yaml",
            Language::Toml => "toml",
            Language::Markdown => "markdown",
            Language::Sql => "sql",
            Language::Css => "css",
            Language::Scss => "scss",
            Language::Less => "less",
            Language::Html => "html",
            Language::Vue => "vue",
            Language::Svelte => "svelte",
            Language::Astro => "astro",
            Language::Angular => "angular",
            Language::Jinja => "jinja",
            Language::Vento => "vento",
            Language::Mustache => "mustache",
            Language::Xml => "xml",
            Language::GraphQl => "graphql",
            Language::Nix => "nix",
            Language::Shell => "shell",
            Language::Dockerfile => "dockerfile",
            Language::Go => "go",
            Language::Java => "java",
            Language::Kotlin => "kotlin",
            Language::Ruby => "ruby",
            Language::Php => "php",
            Language::R => "r",
            Language::Elixir => "elixir",
            Language::C => "c",
            Language::Cpp => "cpp",
            Language::Rust => "rust",
            Language::Proto => "proto",
            Language::Other(s) => s.as_str(),
        }
    }

    /// Detect a language from a file path (special filenames first, then extension).
    /// Returns `None` for unknown extensions; the tree-sitter tier (M5) provides a
    /// secondary fallback for those.
    pub fn from_path(path: &Path) -> Option<Language> {
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            let lower = name.to_ascii_lowercase();
            if lower == "dockerfile"
                || lower.starts_with("dockerfile.")
                || lower.ends_with(".dockerfile")
            {
                return Some(Language::Dockerfile);
            }
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())?
            .to_ascii_lowercase();
        let lang = match ext.as_str() {
            "py" | "pyi" => Language::Python,
            "js" | "cjs" | "mjs" => Language::JavaScript,
            "jsx" => Language::Jsx,
            "ts" | "cts" | "mts" => Language::TypeScript,
            "tsx" => Language::Tsx,
            "json" => Language::Json,
            "jsonc" => Language::Jsonc,
            "yaml" | "yml" => Language::Yaml,
            "toml" => Language::Toml,
            "md" | "markdown" => Language::Markdown,
            "sql" => Language::Sql,
            "css" => Language::Css,
            "scss" => Language::Scss,
            "less" => Language::Less,
            "html" | "htm" => {
                // Angular component templates follow the `*.component.html`
                // convention. markup_fmt's own `detect_language` applies this only to
                // `.html`; we extend it to `.htm` by analogy (`*.component.htm` is
                // effectively nonexistent in practice, so there is no routing risk).
                if path
                    .file_stem()
                    .is_some_and(|s| s.to_string_lossy().ends_with(".component"))
                {
                    Language::Angular
                } else {
                    Language::Html
                }
            }
            "vue" => Language::Vue,
            "svelte" => Language::Svelte,
            "astro" => Language::Astro,
            "jinja" | "jinja2" | "j2" | "twig" | "njk" => Language::Jinja,
            "vto" => Language::Vento,
            "mustache" | "hbs" | "handlebars" => Language::Mustache,
            "xml" | "svg" | "wsdl" | "xsd" | "xslt" | "xsl" => Language::Xml,
            "graphql" | "gql" => Language::GraphQl,
            "nix" => Language::Nix,
            "sh" | "bash" | "zsh" => Language::Shell,
            "go" => Language::Go,
            "java" => Language::Java,
            "kt" | "kts" => Language::Kotlin,
            "rb" => Language::Ruby,
            "php" => Language::Php,
            "r" => Language::R,
            "ex" | "exs" => Language::Elixir,
            "c" | "h" => Language::C,
            "cpp" | "cc" | "cxx" | "hpp" | "hh" => Language::Cpp,
            "rs" => Language::Rust,
            "proto" => Language::Proto,
            _ => return None,
        };
        Some(lang)
    }

    /// Opinionated default indentation width for this language.
    pub fn default_indent_width(&self) -> usize {
        match self {
            Language::JavaScript
            | Language::TypeScript
            | Language::Jsx
            | Language::Tsx
            | Language::Json
            | Language::Jsonc
            | Language::Yaml
            | Language::Ruby
            | Language::R
            | Language::Css
            | Language::Scss
            | Language::Less
            | Language::Html
            | Language::Vue
            | Language::Svelte
            | Language::Astro
            | Language::Angular
            | Language::Jinja
            | Language::Vento
            | Language::Mustache
            | Language::Xml
            | Language::GraphQl => 2,
            _ => 4,
        }
    }
}
