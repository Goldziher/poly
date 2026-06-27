//! Backend implementations. Each backend is a self-contained module implementing
//! [`crate::engine::Engine`]. New backends are added here and wired into
//! [`crate::registry::engines_for`].

pub mod graphql;
pub mod mago;
pub mod malva;
pub mod markup_fmt;
pub mod nixfmt;
pub mod oxc;
pub mod rubyfmt;
pub mod ruff;
pub mod rumdl;
pub mod sqruff;
pub mod taplo;
pub mod treesitter;
pub mod typos;
pub mod yaml;
