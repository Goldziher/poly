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
        Language::Markdown | Language::Mdx => vec![Box::new(RumdlEngine)],
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

#[cfg(test)]
mod tests {
    //! Structural guard against the failure family described in
    //! `tests/version_audit.rs`: that test hand-lists which engines it checks
    //! against `Cargo.lock`, and `astgrep` was once left off that list —
    //! `astgrep` kept advertising a stale `tree-sitter-language-pack` version
    //! while the audit stayed green, because nothing forced the list to stay
    //! exhaustive. `engines_for` is `pub(crate)`, so `tests/version_audit.rs`
    //! (an external integration-test binary that only sees the public API)
    //! cannot walk it directly — this in-crate unit test does, and cross-checks
    //! the result against what `version_audit.rs` actually declares.
    use std::collections::BTreeSet;
    use std::path::Path;

    use regex::Regex;

    use super::engines_for;
    use crate::language::Language;

    /// Every concrete (non-[`Language::Other`]) [`Language`] variant, used to
    /// walk every arm of [`engines_for`]. Kept from silently narrowing by
    /// `assert_all_language_variants_listed` below: adding a new `Language`
    /// variant without adding it to both that match and this list fails to
    /// compile, rather than letting a newly registry-wired engine for the new
    /// language slip past this audit unnoticed.
    fn all_known_languages() -> Vec<Language> {
        vec![
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
        ]
    }

    /// Compile-time companion to `all_known_languages`: an exhaustive match
    /// (no wildcard arm) over every `Language` variant. Never called — its only
    /// purpose is that adding a variant to the enum without adding it here
    /// stops this file compiling, forcing whoever adds it to also decide
    /// whether `all_known_languages` (and therefore this audit) needs it.
    #[allow(dead_code)]
    fn assert_all_language_variants_listed(language: &Language) {
        match language {
            Language::Python
            | Language::JavaScript
            | Language::TypeScript
            | Language::Jsx
            | Language::Tsx
            | Language::Json
            | Language::Jsonc
            | Language::Yaml
            | Language::Toml
            | Language::Markdown
            | Language::Mdx
            | Language::Sql
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
            | Language::Hcl
            | Language::Nix
            | Language::Shell
            | Language::Dockerfile
            | Language::Go
            | Language::Java
            | Language::Kotlin
            | Language::Ruby
            | Language::Php
            | Language::R
            | Language::Elixir
            | Language::C
            | Language::Cpp
            | Language::Rust
            | Language::Proto
            | Language::Zig
            | Language::Swift
            | Language::Dart
            | Language::Gleam
            | Language::CSharp
            | Language::Other(_) => {}
        }
    }

    /// Native-toolchain backends (`native_tool.rs`) wrap an external
    /// first-party CLI (`gofmt`, `rustfmt`, …), not a pinned Rust crate, so
    /// `tests/version_audit.rs` has no `Cargo.lock` entry to check them
    /// against. This is the one visible, explicit exemption list — engines
    /// land here on purpose, never by being left off both lists.
    ///
    /// Catalog backends (`engines/catalog_tool`) are the other legitimately
    /// exempt family, wrapping arbitrary user-configured external tools; they
    /// never appear in `engines_for` at all (they are built from `poly.toml`
    /// in `runner/plan.rs`, not wired into the registry), so they cannot reach
    /// this traversal and need no entry here.
    const NATIVE_TOOLCHAIN_ENGINES: &[&str] = &[
        "gofmt",
        "rustfmt",
        "zigfmt",
        "shfmt",
        "shellcheck",
        "google-java-format",
        "ktfmt",
        "styler",
        "swift-format",
        "dartfmt",
        "gleamfmt",
    ];

    /// Engine names `tests/version_audit.rs` declares via `check("name", ...)`.
    /// Read from the sibling file's source text rather than duplicating the
    /// list here, so the two files cannot drift apart.
    fn audited_engine_names() -> BTreeSet<String> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/version_audit.rs");
        let source = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        let pattern = Regex::new(r#"check\(\s*"([^"]+)""#).expect("valid regex");
        pattern
            .captures_iter(&source)
            .map(|capture| capture[1].to_owned())
            .collect()
    }

    #[test]
    fn every_registered_engine_is_audited_or_declared_exempt() {
        let mut registered: BTreeSet<&'static str> = BTreeSet::new();
        for language in all_known_languages() {
            for engine in engines_for(&language) {
                registered.insert(engine.name());
            }
        }
        assert!(
            !registered.is_empty(),
            "collected zero engines from the registry; the traversal is broken, not the audit"
        );

        let audited = audited_engine_names();
        assert!(
            !audited.is_empty(),
            "collected zero `check(...)` entries from tests/version_audit.rs; the \
             sibling-file scan is broken, not the audit"
        );

        for name in &registered {
            let is_exempt = NATIVE_TOOLCHAIN_ENGINES.contains(name);
            let is_audited = audited.contains(*name);
            assert!(
                is_exempt || is_audited,
                "engine `{name}` is wired into `registry::engines_for` but is neither \
                 checked in tests/version_audit.rs nor declared exempt in \
                 registry::tests::NATIVE_TOOLCHAIN_ENGINES. If it wraps a pinned Rust crate, \
                 add a `check(\"{name}\", ...)` entry naming that crate; if it wraps an \
                 external CLI with no pinned crate, add \"{name}\" to \
                 NATIVE_TOOLCHAIN_ENGINES instead.",
            );
        }
    }
}
