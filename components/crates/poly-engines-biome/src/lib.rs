//! The biome engine family: CSS and GraphQL lint, and the `biome` component serving it.

pub mod common;
pub mod css;
pub mod graphql;

pub use css::BiomeCssEngine;
pub use graphql::BiomeGraphqlEngine;

use poly_engine_api::engine::Engine;
use poly_engine_api::language::Language;
use poly_engines::{ComponentCallError, PolyComponent, Producer};

/// The engines this family registers for `language`; empty when the family does not handle it.
///
/// A bundled binary hands this to `poly_core::register_engine_provider`, which appends the result
/// after the language's built-in engines (CSS: malva, then biome; GraphQL: graphql, then biome).
pub fn engines_for(language: &Language) -> Vec<Box<dyn Engine>> {
    match language {
        Language::Css | Language::Scss => vec![Box::new(BiomeCssEngine)],
        Language::GraphQl => vec![Box::new(BiomeGraphqlEngine)],
        _ => Vec::new(),
    }
}

/// The `biome` component: biome's CSS and GraphQL lint behind the component protocol.
pub struct BiomeComponent(Producer);

impl Default for BiomeComponent {
    fn default() -> Self {
        Self(Producer::new(
            env!("CARGO_PKG_VERSION"),
            vec![Box::new(BiomeCssEngine), Box::new(BiomeGraphqlEngine)],
        ))
    }
}

impl PolyComponent for BiomeComponent {
    fn describe(&self) -> Result<Vec<u8>, ComponentCallError> {
        self.0.describe()
    }

    fn call(&self, header: &[u8], body: &[u8]) -> Result<Vec<u8>, ComponentCallError> {
        self.0.call(header, body)
    }
}
