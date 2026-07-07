//! `poly hooks` — run git hooks declared in `poly.toml`'s `[hooks]` table.
//!
//! poly drives a **native, in-process** hook runner (`poly-hooks`): the parsed
//! `[hooks]` config is lowered ([`lower`]) into the runner's model and executed
//! by [`poly_hooks::run`]. There is no external hook engine and no generated
//! YAML — poly's own tools (`[hooks.builtin]`) lower to commands invoking the
//! running `poly` binary, and inline jobs (`[[hooks.<stage>.jobs]]`, `.commands`,
//! `.scripts`) lower to per-stage hooks.

pub mod checks;
pub mod commands;
pub mod lower;
pub mod workspace_lint;

pub use commands::{HooksArgs, run_hooks};
