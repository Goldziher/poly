//! The ready-made component implementations: one type per engine family, each a [`Producer`]
//! over that family's engines.
//!
//! `alef` names these types as the `implementation` of a component's contract, so each is
//! `Default`-constructible and takes its version from this crate, which inherits the workspace
//! version the component artifacts are published under.

#[cfg(feature = "sql")]
mod sql {
    use crate::{ComponentCallError, PolyComponent, Producer};

    /// The `sql` component: sqruff behind the component protocol.
    pub struct SqlComponent(Producer);

    impl Default for SqlComponent {
        fn default() -> Self {
            Self(Producer::new(
                env!("CARGO_PKG_VERSION"),
                vec![Box::new(crate::sql::SqruffEngine)],
            ))
        }
    }

    impl PolyComponent for SqlComponent {
        fn describe(&self) -> Result<Vec<u8>, ComponentCallError> {
            self.0.describe()
        }

        fn call(&self, header: &[u8], body: &[u8]) -> Result<Vec<u8>, ComponentCallError> {
            self.0.call(header, body)
        }
    }
}

#[cfg(feature = "sql")]
pub use sql::SqlComponent;
