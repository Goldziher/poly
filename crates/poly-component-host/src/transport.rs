//! The byte-level seam between the host and a loaded component.

use std::sync::Arc;

use alef_component_runtime::ComponentManager;

/// Message of the error [`AlefConnector`] returns after a component was downloaded and verified.
pub const TRANSPORT_NOT_GENERATED: &str = "component transport not generated";

/// The two calls the component protocol needs; see [`poly_component_protocol`] for the framing.
///
/// `Send + Sync` because the host calls a component from its rayon worker pool.
pub trait ComponentTransport: Send + Sync {
    /// The component's encoded [`Describe`](poly_component_protocol::Describe).
    ///
    /// # Errors
    ///
    /// When the component cannot be reached.
    fn describe(&self) -> anyhow::Result<Vec<u8>>;

    /// One call: `header` is an encoded [`Request`](poly_component_protocol::Request), `body`
    /// carries file content. Returns an encoded [`Response`](poly_component_protocol::Response).
    ///
    /// # Errors
    ///
    /// When the call itself failed (not when the engine reported an error: that is a
    /// `Response`).
    fn call(&self, header: &[u8], body: &[u8]) -> anyhow::Result<Vec<u8>>;
}

/// Produces the [`ComponentTransport`] for a component the manager can install.
pub trait TransportConnector: Send + Sync {
    /// Install (download, verify, cache) `component_id` through `manager` and connect to it.
    ///
    /// # Errors
    ///
    /// When the component cannot be installed or connected.
    fn connect(&self, manager: &ComponentManager, component_id: &str) -> anyhow::Result<Arc<dyn ComponentTransport>>;
}

/// Connects through alef's runtime.
///
/// Installation is real: the archive is downloaded, its size, SHA-256 and signature are verified
/// against the embedded lock and the library is cached. The bytes-contract proxy that would
/// instantiate the library is emitted by `alef generate` (xberg-io/alef#825) and is not part of
/// the 0.118 runtime, so a verified component currently ends in a
/// [`TRANSPORT_NOT_GENERATED`] error. The host remembers it like any load failure.
#[derive(Debug, Clone, Copy, Default)]
pub struct AlefConnector;

impl TransportConnector for AlefConnector {
    fn connect(&self, manager: &ComponentManager, component_id: &str) -> anyhow::Result<Arc<dyn ComponentTransport>> {
        manager.prefetch(&[component_id])?;
        anyhow::bail!(
            "{TRANSPORT_NOT_GENERATED}: `{component_id}` was downloaded and verified, but its contract proxy has \
             not been generated"
        )
    }
}
