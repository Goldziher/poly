//! The byte-level seam between the host and a loaded component.

use std::collections::HashSet;
use std::sync::Arc;

use alef_component_runtime::{ComponentManager, LoadedComponent};
use poly_component_protocol::sidecar::{CONTRACT, METHOD_CALL, METHOD_DESCRIBE, pack_call};
use poly_engines::PolyComponent;

use crate::components_gen::{POLY_ENTRY_SYMBOL, PolyProxy};

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
/// its signature), cached and loaded with `ComponentManager::ensure_contract`. A dylib component is
/// then driven through the generated `PolyProxy` and the `PolyComponent` contract; the proxy comes
/// from `alef generate` (see `alef.toml`), so the contract hash it carries is the one the lock
/// records: a component built against a different contract fails to load rather than misbehave.
///
/// A component named in [`AlefConnector::with_sidecars`] is an executable: the manager spawns it
/// (see `ComponentManager::with_stdio_component`, which [`ComponentHost`](crate::ComponentHost)
/// sets for the same names) and the two protocol calls travel as `invoke_bytes` payloads, packed as
/// [`poly_component_protocol::sidecar`] describes. Engines cannot tell the two apart.
#[derive(Debug, Clone, Default)]
pub struct AlefConnector {
    sidecars: HashSet<String>,
}

impl AlefConnector {
    /// A connector that treats `ids` as sidecar executables and every other component as a dylib.
    #[must_use]
    pub fn with_sidecars(ids: impl IntoIterator<Item = String>) -> Self {
        Self {
            sidecars: ids.into_iter().collect(),
        }
    }
}

impl TransportConnector for AlefConnector {
    fn connect(&self, manager: &ComponentManager, component_id: &str) -> anyhow::Result<Arc<dyn ComponentTransport>> {
        let loaded = manager.ensure_contract(component_id, CONTRACT, POLY_ENTRY_SYMBOL)?;
        if self.sidecars.contains(component_id) {
            return Ok(Arc::new(SidecarTransport { loaded }));
        }
        let proxy = PolyProxy::new(&loaded).map_err(anyhow::Error::msg)?;
        Ok(Arc::new(ProxyTransport { proxy }))
    }
}

/// The byte-level seam over a sidecar executable: alef's framed stdio, one child, calls serialized.
///
/// A timeout, a crashed child or a malformed frame invalidates the connection inside alef (the
/// child is killed and reaped), so every later call fails with `sidecar_stopped`; this transport
/// never restarts it.
struct SidecarTransport {
    loaded: Arc<LoadedComponent>,
}

impl ComponentTransport for SidecarTransport {
    fn describe(&self) -> anyhow::Result<Vec<u8>> {
        Ok(self.loaded.invoke_bytes(METHOD_DESCRIBE, &[])?)
    }

    fn call(&self, header: &[u8], body: &[u8]) -> anyhow::Result<Vec<u8>> {
        let payload = pack_call(header, body)?;
        Ok(self.loaded.invoke_bytes(METHOD_CALL, &payload)?)
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
