//! The seam between poly and a frontend component (the MCP server).
//!
//! A frontend runs inside the core process but links none of its engines. Everything it does for
//! its client is a **host service** call: the component sends a service name and request bytes
//! through the component ABI's service table, and the host answers with response bytes. Both
//! sides encode them as JSON, using the types in [`wire`] and [`dto`]; [`ServiceCaller`] is the
//! one-method trait the frontend is written against, so the same server runs over the ABI table
//! (as a downloaded component), over an in-process host (the bundled build) or over a fake in a
//! test.

pub mod dto;
pub mod wire;

pub use wire::{ServiceCaller, call};
