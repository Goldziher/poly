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
    /// HCL / Terraform configuration language.
    Hcl,
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
    /// Zig.
    Zig,
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
            Language::Hcl => "hcl",
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
            Language::Zig => "zig",
            Language::Other(s) => s.as_str(),
        }
    }

    /// Detect a language from a file path (special filenames first, then extension).
    /// Returns `None` for unknown extensions; the tree-sitter tier (M5) provides a
    /// secondary fallback for those.
    pub fn from_path(path: &Path) -> Option<Language> {
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            let lower = name.to_ascii_lowercase();
            if lower == "dockerfile" || lower.starts_with("dockerfile.") || lower.ends_with(".dockerfile") {
                return Some(Language::Dockerfile);
            }
        }
        let ext = path.extension().and_then(|e| e.to_str())?.to_ascii_lowercase();
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
            "hcl" | "tf" | "tfvars" => Language::Hcl,
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
            "zig" => Language::Zig,
            _ => return None,
        };
        Some(lang)
    }

    /// Map a catalog (mdsf) language identifier to a poly [`Language`].
    ///
    /// Used to route an enabled `[tools.<name>]` catalog tool to the files it
    /// handles: a tool declaring `languages = ["go"]` is routed to
    /// [`Language::Go`]. Names with a dedicated tier-1 variant map to it
    /// (handling spelling differences like `c++` / `c#` and the template-family
    /// aliases); anything else maps to [`Language::Other`] so a tool for a
    /// long-tail language still routes to files the tree-sitter tier detects
    /// under that same id.
    pub fn from_catalog_name(name: &str) -> Language {
        match name {
            "python" => Language::Python,
            "javascript" => Language::JavaScript,
            "typescript" => Language::TypeScript,
            "jsx" => Language::Jsx,
            "tsx" => Language::Tsx,
            "json" => Language::Json,
            "json5" | "jsonc" => Language::Jsonc,
            "yaml" => Language::Yaml,
            "toml" => Language::Toml,
            "markdown" => Language::Markdown,
            "sql" => Language::Sql,
            "css" => Language::Css,
            "scss" => Language::Scss,
            "less" => Language::Less,
            "html" => Language::Html,
            "angular" => Language::Angular,
            "vue" => Language::Vue,
            "svelte" => Language::Svelte,
            "astro" => Language::Astro,
            "jinja" | "twig" | "nunjucks" | "django" => Language::Jinja,
            "vento" => Language::Vento,
            "handlebars" | "mustache" => Language::Mustache,
            "xml" => Language::Xml,
            "graphql" => Language::GraphQl,
            "hcl" | "terraform" => Language::Hcl,
            "nix" => Language::Nix,
            "bash" | "shell" | "sh" | "zsh" => Language::Shell,
            "docker" | "dockerfile" => Language::Dockerfile,
            "go" => Language::Go,
            "java" => Language::Java,
            "kotlin" => Language::Kotlin,
            "ruby" => Language::Ruby,
            "php" => Language::Php,
            "r" => Language::R,
            "elixir" => Language::Elixir,
            "c" => Language::C,
            "c++" | "cpp" => Language::Cpp,
            // `c#` / `csharp`: poly has no `CSharp` variant yet, so these are
            // intentionally left to fall through to `Language::Other`. clang-format
            // (and other C# catalog tools) therefore will not route to `.cs` files
            // until a dedicated `CSharp` variant exists — out of scope here.
            "rust" => Language::Rust,
            "protobuf" | "proto" => Language::Proto,
            "zig" => Language::Zig,
            // mdsf spells these with a long form; poly's tree-sitter detection
            // assigns the canonical pack id (`makefile` aliases to `make`, and
            // `.vim` files detect as `vim`). Normalise both so an enabled catalog
            // tool routes to the files discovery actually labels.
            "make" | "makefile" => Language::Other("make".to_string()),
            "vim" | "vimscript" => Language::Other("vim".to_string()),
            other => Language::Other(other.to_string()),
        }
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
            | Language::GraphQl
            | Language::Hcl => 2,
            _ => 4,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Language;

    #[test]
    fn from_catalog_name_maps_angular_and_vento_to_their_variants() {
        assert_eq!(Language::from_catalog_name("angular"), Language::Angular);
        assert_eq!(Language::from_catalog_name("vento"), Language::Vento);
    }

    #[test]
    fn from_catalog_name_normalises_make_aliases_to_pack_id() {
        // mdsf uses both `make` and `makefile`; the tree-sitter pack's canonical
        // id is `make` (it aliases `makefile` -> `make`), so both must land there.
        let expected = Language::Other("make".to_string());
        assert_eq!(Language::from_catalog_name("make"), expected);
        assert_eq!(Language::from_catalog_name("makefile"), expected);
    }

    #[test]
    fn from_catalog_name_normalises_vimscript_to_pack_id() {
        // mdsf spells it `vimscript`; the pack detects `.vim` files as `vim`.
        let expected = Language::Other("vim".to_string());
        assert_eq!(Language::from_catalog_name("vimscript"), expected);
        assert_eq!(Language::from_catalog_name("vim"), expected);
    }

    #[test]
    fn from_catalog_name_leaves_csharp_unmapped() {
        // No `CSharp` variant exists, so C# stays `Other` (see the inline note).
        assert_eq!(Language::from_catalog_name("c#"), Language::Other("c#".to_string()));
        assert_eq!(
            Language::from_catalog_name("csharp"),
            Language::Other("csharp".to_string())
        );
    }
}
