//! The byte-level seam between the host and a loaded component.

use std::sync::Arc;

use alef_component_runtime::ComponentManager;
use poly_engines::PolyComponent;

use crate::components_gen::{POLY_ENTRY_SYMBOL, PolyProxy};

/// Name of the contract every poly component provides, as declared in `alef.toml`.
const CONTRACT: &str = "poly";

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
/// The component is downloaded, verified against the lock (size, SHA-256 and, for a signed lock,
/// its signature), cached and loaded with `ComponentManager::ensure_contract`; the generated
/// `PolyProxy` then drives it through the `PolyComponent` contract. The proxy comes from
/// `alef generate` (see `alef.toml`), so the contract hash it carries is the one the lock records:
/// a component built against a different contract fails to load rather than misbehave.
#[derive(Debug, Clone, Copy, Default)]
pub struct AlefConnector;

impl TransportConnector for AlefConnector {
    fn connect(&self, manager: &ComponentManager, component_id: &str) -> anyhow::Result<Arc<dyn ComponentTransport>> {
        let loaded = manager.ensure_contract(component_id, CONTRACT, POLY_ENTRY_SYMBOL)?;
        let proxy = PolyProxy::new(&loaded).map_err(anyhow::Error::msg)?;
        Ok(Arc::new(ProxyTransport { proxy }))
    }
}

/// The byte-level seam over the generated contract proxy.
struct ProxyTransport {
    proxy: PolyProxy,
}

impl ComponentTransport for ProxyTransport {
    fn describe(&self) -> anyhow::Result<Vec<u8>> {
        Ok(self.proxy.describe()?)
    }

    fn call(&self, header: &[u8], body: &[u8]) -> anyhow::Result<Vec<u8>> {
        Ok(self.proxy.call(header, body)?)
    }
}
