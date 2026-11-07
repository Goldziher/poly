//! Facade over poly's engine family crates.
//!
//! Each family is behind a feature of the same name and re-exported under it
//! (`poly_engines::sql`, `poly_engines::python`, ...). The facade also owns the
//! [`PolyComponent`] contract a component implements to be driven by the host.

mod component;
mod components;
pub mod producer;

pub use component::{ComponentCallError, PolyComponent, PolyFrontend};
#[cfg(feature = "astgrep")]
pub use components::AstgrepComponent;
#[cfg(feature = "mcp")]
pub use components::McpComponent;
#[cfg(feature = "misc")]
pub use components::MiscComponent;
#[cfg(feature = "nix")]
pub use components::NixComponent;
#[cfg(feature = "php")]
pub use components::PhpComponent;
#[cfg(feature = "python")]
pub use components::PythonComponent;
#[cfg(feature = "sql")]
pub use components::SqlComponent;
#[cfg(feature = "typos")]
pub use components::TyposComponent;
#[cfg(feature = "uncomment")]
pub use components::UncommentComponent;
pub use producer::Producer;

#[cfg(feature = "astgrep")]
pub use poly_engines_astgrep as astgrep;
#[cfg(feature = "misc")]
pub use poly_engines_misc as misc;
#[cfg(feature = "nix")]
pub use poly_engines_nix as nix;
#[cfg(feature = "php")]
pub use poly_engines_php as php;
#[cfg(feature = "python")]
pub use poly_engines_python as python;
#[cfg(feature = "sql")]
pub use poly_engines_sql as sql;
#[cfg(feature = "typos")]
pub use poly_engines_typos as typos;
#[cfg(feature = "uncomment")]
pub use poly_engines_uncomment as uncomment;
