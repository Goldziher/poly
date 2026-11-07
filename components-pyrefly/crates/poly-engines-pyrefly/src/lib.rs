//! The pyrefly engine family: Python type checking with pyrefly, and the `pyrefly` component
//! serving it.

mod check;
mod settings;

pub use check::{ENGINE_NAME, PyreflyEngine};

use poly_engines::{ComponentCallError, PolyComponent, Producer};

/// The `pyrefly` component: the pyrefly project check behind the component protocol.
pub struct PyreflyComponent(Producer);

impl Default for PyreflyComponent {
    fn default() -> Self {
        Self(Producer::new(env!("CARGO_PKG_VERSION"), vec![Box::new(PyreflyEngine)]))
    }
}

impl PolyComponent for PyreflyComponent {
    fn describe(&self) -> Result<Vec<u8>, ComponentCallError> {
        self.0.describe()
    }

    fn call(&self, header: &[u8], body: &[u8]) -> Result<Vec<u8>, ComponentCallError> {
        self.0.call(header, body)
    }
}
