//! The Ruby engine family: rubyfmt formatting, and the `ruby` component serving it.

pub mod rubyfmt;

pub use rubyfmt::RubyfmtEngine;

use poly_engine_api::engine::Engine;
use poly_engine_api::language::Language;
use poly_engines::{ComponentCallError, PolyComponent, Producer};

/// The Ruby family's engines for `language`: rubyfmt for Ruby, none for any other language.
///
/// Shaped as an engine provider so a bundled binary chains it with the other families' functions.
#[must_use]
pub fn engines_for(language: &Language) -> Vec<Box<dyn Engine>> {
    match language {
        Language::Ruby => vec![Box::new(RubyfmtEngine)],
        _ => Vec::new(),
    }
}

/// The `ruby` component: rubyfmt behind the component protocol.
pub struct RubyComponent(Producer);

impl Default for RubyComponent {
    fn default() -> Self {
        Self(Producer::new(env!("CARGO_PKG_VERSION"), vec![Box::new(RubyfmtEngine)]))
    }
}

impl PolyComponent for RubyComponent {
    fn describe(&self) -> Result<Vec<u8>, ComponentCallError> {
        self.0.describe()
    }

    fn call(&self, header: &[u8], body: &[u8]) -> Result<Vec<u8>, ComponentCallError> {
        self.0.call(header, body)
    }
}
