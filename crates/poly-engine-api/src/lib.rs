//! The `Engine` contract and the helpers every poly engine shares.
//!
//! Both `poly-core` and each engine family depend on this crate; it deliberately depends on
//! neither, and its dependency tree contains no engine crates (enforced by `tests/no_engine_deps.rs`).
//! `poly-core` re-exports everything here at its historical paths.

pub mod component_wire;
pub mod config;
pub mod css_hacks;
pub mod engine;
pub mod engine_guard;
pub mod host_hooks;
pub mod language;
pub mod line_index;
pub mod option_probe;
pub mod project;
pub mod rule_config;
pub mod template;
