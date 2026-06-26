//! Backend implementations. Each backend is a self-contained module implementing
//! [`crate::engine::Engine`]. New backends are added here and wired into
//! [`crate::registry::engines_for`].

pub mod whitespace;
