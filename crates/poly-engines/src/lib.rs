//! Facade over poly's engine family crates.
//!
//! Each family is behind a feature of the same name and re-exported under it
//! (`poly_engines::sql`, `poly_engines::python`, ...). The facade also owns the
//! [`PolyComponent`] contract a component implements to be driven by the host.

mod component;

pub use component::{ComponentCallError, PolyComponent};

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
