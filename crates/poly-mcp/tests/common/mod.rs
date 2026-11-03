//! Shared setup: the server over the real host services, in process.
//!
//! [`Services`] is what the `poly` binary registers with the component ABI, so a server built
//! here takes the exact wire path a downloaded `mcp` component takes (encode, host service,
//! decode). Only the transport differs, and the ABI's service table is alef's own.

use std::path::PathBuf;
use std::sync::Arc;

use poly_host_services::{ComponentsProvider, Services};
use poly_mcp::PolyMcpServer;
use poly_mcp::identity::ExecutableWatch;

/// A server over the host services.
// Each test binary compiles this module on its own, and not every one uses every helper.
#[allow(dead_code)]
pub fn server(config: Option<PathBuf>) -> PolyMcpServer {
    PolyMcpServer::new(Arc::new(Services::new()), config)
}

/// A server whose executable watch is `watch`.
// Each test binary compiles this module on its own, and only one of them replaces the watch.
#[allow(dead_code)]
pub fn server_watching(config: Option<PathBuf>, watch: ExecutableWatch) -> PolyMcpServer {
    PolyMcpServer::with_executable_watch(Arc::new(Services::new()), config, watch)
}

/// A server over host services whose component answers come from `provider`.
// Only the component tests supply one.
#[allow(dead_code)]
pub fn server_with_components(provider: Arc<dyn ComponentsProvider>) -> PolyMcpServer {
    PolyMcpServer::new(Arc::new(Services::new().with_components(provider)), None)
}
