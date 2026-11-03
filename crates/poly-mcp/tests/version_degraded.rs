//! The `version` tool answers even when the host cannot report engines or components, since it
//! is the tool a client calls to find out why everything else stopped. Its own binary: the
//! identity and schema OnceLocks are process-wide, and this host fails the lookups that fill them.

use std::sync::Arc;

use poly_frontend_api::ServiceCaller;
use poly_frontend_api::wire::service;
use poly_host_services::Services;
use poly_mcp::PolyMcpServer;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ClientCapabilities, ClientConfig, Implementation};
use serde_json::Value;

/// The real services, except that the engine and component lookups fail.
struct Failing(Services);

impl ServiceCaller for Failing {
    fn call(&self, name: &str, request: &[u8]) -> Result<Vec<u8>, String> {
        if name == service::ENGINES || name == service::COMPONENTS {
            return Err("component host detached".to_string());
        }
        self.0.call(name, request)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn version_reports_a_host_failure_instead_of_failing() {
    let (server_io, client_io) = tokio::io::duplex(1 << 16);
    let server_task = tokio::spawn(async move {
        let server = PolyMcpServer::new(Arc::new(Failing(Services::new())), None);
        let service = server.serve(tokio::io::split(server_io)).await.unwrap();
        service.waiting().await.unwrap();
    });
    let client = ClientConfig::new(
        ClientCapabilities::builder().build(),
        Implementation::new("poly-mcp-test", "0.0.0"),
    )
    .serve(tokio::io::split(client_io))
    .await
    .unwrap();

    let result = client.call_tool(CallToolRequestParams::new("version")).await.unwrap();

    assert_ne!(result.is_error, Some(true), "{result:?}");
    let structured = result.structured_content.as_ref().expect("structured content");
    let status = structured["executable_status"].as_str().unwrap();
    assert!(status.contains("component host detached"), "{status}");
    assert!(
        matches!(&structured["components"], Value::Null) || structured["components"] == Value::Array(Vec::new()),
        "no components to report: {structured}"
    );
    client.cancel().await.unwrap();
    let _ = server_task.await;
}
