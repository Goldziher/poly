//! The services poly's core exposes to a frontend component.
//!
//! [`Services`] answers the service names of [`poly_frontend_api::wire::service`]: it decodes the
//! JSON request, runs the same `poly-core` pipeline the CLI runs (see [`ops`]) and encodes the
//! reply. It links the engines the host binary links and nothing of the frontend: no `rmcp`, no
//! `tokio`. The component ABI's `HostServices` and the in-process (bundled) server both reach it
//! through [`ServiceCaller`].
//!
//! Config resolution is network-free here, as it always was for the MCP server: a remote git
//! `extends` base is not fetched (see [`ops`]).

pub mod ops;
mod services;

pub use poly_frontend_api::ServiceCaller;
pub use services::{ComponentsProvider, Services};
