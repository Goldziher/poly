//! The web engine family: oxc (JavaScript, TypeScript, JSON lint and format), taplo and
//! `oxc_formatter_toml` (TOML), `oxc_formatter_yaml` (YAML), malva (CSS, SCSS, Less), markup_fmt
//! (HTML and template languages), and Markdown through rumdl, adrlint and `oxc_formatter_markdown`. skif lints
//! JavaScript and TypeScript workspaces as a project check.
//!
//! The family lives in the `components/` workspace because its oxc crates are git dependencies.

pub mod adrlint;
mod core_options;
pub mod malva;
mod markdown_embed;
mod markdown_fmt;
pub mod markup_fmt;
pub mod oxc;
pub mod rumdl;
pub mod skif;
pub mod taplo;
pub mod yaml;

// Spelled with `crate::` because `adrlint`, `malva`, `markup_fmt` and `taplo` are also the names of the
// external crates the modules wrap, and a bare path in a `use` would be ambiguous between the two.
pub use crate::adrlint::AdrlintEngine;
pub use crate::malva::MalvaEngine;
pub use crate::markup_fmt::MarkupFmtEngine;
pub use crate::oxc::OxcEngine;
pub use crate::rumdl::RumdlEngine;
pub use crate::skif::SkifEngine;
pub use crate::taplo::TaploEngine;
pub use crate::yaml::YamlEngine;

use poly_engine_api::engine::Engine;
use poly_engine_api::language::Language;
use poly_engines::{ComponentCallError, PolyComponent, Producer};

/// The web family's engines for `language`, in registration order; empty for any other language.
///
/// Shaped as an engine provider so a bundled binary chains it with the other families' functions.
/// `poly.toml` key checking for TOML stays a core engine; only the TOML formatter and linter are
/// served here.
#[must_use]
pub fn engines_for(language: &Language) -> Vec<Box<dyn Engine>> {
    match language {
        Language::JavaScript
        | Language::TypeScript
        | Language::Jsx
        | Language::Tsx
        | Language::Json
        | Language::Jsonc => vec![Box::new(OxcEngine), Box::new(SkifEngine)],
        Language::Toml => vec![Box::new(TaploEngine::new()), Box::new(SkifEngine)],
        Language::Markdown => vec![Box::new(RumdlEngine), Box::new(AdrlintEngine)],
        Language::Mdx => vec![Box::new(RumdlEngine)],
        Language::Yaml => vec![Box::new(YamlEngine), Box::new(SkifEngine)],
        Language::Css | Language::Scss | Language::Less => vec![Box::new(MalvaEngine)],
        Language::Html
        | Language::Vue
        | Language::Svelte
        | Language::Astro
        | Language::Angular
        | Language::Jinja
        | Language::Vento
        | Language::Mustache
        | Language::Xml => vec![Box::new(MarkupFmtEngine)],
        _ => Vec::new(),
    }
}

/// The `web` component: every engine of this family behind the component protocol.
pub struct WebComponent(Producer);

impl Default for WebComponent {
    fn default() -> Self {
        Self(Producer::new(
            env!("CARGO_PKG_VERSION"),
            vec![
                Box::new(AdrlintEngine),
                Box::new(MalvaEngine),
                Box::new(MarkupFmtEngine),
                Box::new(OxcEngine),
                Box::new(RumdlEngine),
                Box::new(SkifEngine),
                Box::new(TaploEngine::new()),
                Box::new(YamlEngine),
            ],
        ))
    }
}

impl PolyComponent for WebComponent {
    fn describe(&self) -> Result<Vec<u8>, ComponentCallError> {
        self.0.describe()
    }

    fn call(&self, header: &[u8], body: &[u8]) -> Result<Vec<u8>, ComponentCallError> {
        self.0.call(header, body)
    }
}
