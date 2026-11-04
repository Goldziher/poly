//! The Ruby engine family: rubyfmt formatting, and the `ruby` component serving it.

pub mod rubyfmt;

pub use rubyfmt::RubyfmtEngine;

use poly_engines::{ComponentCallError, PolyComponent, Producer};

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
