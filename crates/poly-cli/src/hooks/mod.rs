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
pub mod sources;

// The hooks→`poly-hooks` lowering now lives in the shared `poly-workspace` crate
// (so the CLI and the MCP server share one whole-project lint orchestration). It
// is re-exported here so the in-crate call sites keep referring to
// `crate::hooks::lower`. ~keep
pub use poly_workspace::lower;

pub use commands::{HooksArgs, run_hooks};
