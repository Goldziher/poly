//! Tests for the `poly-mcp` server.
//!
//! Three layers:
//! 1. `ops` behaviour — the synchronous engine calls produce the CLI's JSON
//!    contract on a temp fixture.
//! 2. Tool-registry introspection — the expected tool names and annotations
//!    (read-only vs destructive) are registered.
//! 3. An in-process round-trip over a tokio duplex transport: initialize →
//!    tools/list → tools/call.

use std::path::PathBuf;

use poly_mcp::{PolyMcpServer, ops};
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use serde_json::Value;

/// Write a Python file with a real lint defect (unused import → ruff F401) into
/// a temp dir. Trailing whitespace is a `fmt` concern, not a lint one, so a
/// structured linter is used here to exercise the diagnostic contract.
fn fixture_with_defect() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("bad.py"), "import os\n").unwrap();
    dir
}

#[test]
fn lint_emits_diagnostic_contract_json() {
    let dir = fixture_with_defect();
    let path = dir.path().join("bad.py");
    let json = ops::lint(&[path.display().to_string()], &[], None, false).unwrap();
    let parsed: Value = serde_json::from_str(&json).unwrap();
    let results = parsed.as_array().expect("lint json is an array");
    assert!(!results.is_empty(), "expected at least one lint result");
    let diagnostics = results[0]["diagnostics"]
        .as_array()
        .expect("result has diagnostics array");
    let first = &diagnostics[0];
    assert!(first["engine"].is_string(), "diagnostic has engine");
    assert!(first["severity"].is_string(), "diagnostic has severity");
    assert!(first["title"].is_string(), "diagnostic has title");
}

#[test]
fn format_check_reports_changed_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.rs");
    let original = "fn main() {}   \n";
    std::fs::write(&path, original).unwrap();
    let json = ops::format(&[path.display().to_string()], &[], None, false).unwrap();
    let parsed: Value = serde_json::from_str(&json).unwrap();
    let results = parsed.as_array().expect("format json is an array");
    if let Some(first) = results.first() {
        assert!(first["changed"].is_boolean(), "result has changed flag");
        assert!(first.get("path").is_some(), "result has path");
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
}

#[test]
fn explicit_missing_config_is_an_error() {
    let result = ops::lint(&[".".to_string()], &[], Some("/nonexistent/poly.toml"), false);
    assert!(result.is_err(), "missing explicit config should error");
}

#[test]
fn cache_stats_returns_json_object() {
    let json = ops::cache_stats().unwrap();
    let parsed: Value = serde_json::from_str(&json).unwrap();
    assert!(parsed.get("total_bytes").is_some(), "stats has total_bytes");
    assert!(parsed["per_namespace"].is_array(), "stats has per_namespace array");
}

#[test]
fn registered_tools_have_expected_names_and_annotations() {
    let server = PolyMcpServer::new(None);
    let mut names = server.tool_names();
    names.sort();
    assert_eq!(
        names,
        vec![
            "cache_clean",
            "cache_stats",
            "format_check",
            "format_write",
            "lint",
            "lint_fix",
        ]
    );

    for tool in ["lint", "format_check", "cache_stats"] {
        let (read_only, destructive) = server.tool_hints(tool).unwrap();
        assert_eq!(read_only, Some(true), "{tool} should be read-only");
        assert_eq!(destructive, Some(false), "{tool} should not be destructive");
    }
    for tool in ["lint_fix", "format_write", "cache_clean"] {
        let (read_only, destructive) = server.tool_hints(tool).unwrap();
        assert_eq!(read_only, Some(false), "{tool} should not be read-only");
        assert_eq!(destructive, Some(true), "{tool} should be destructive");
    }
}

#[test]
fn server_constructs_with_config_override() {
    let server = PolyMcpServer::new(Some(PathBuf::from("poly.toml")));
    assert_eq!(server.tool_names().len(), 6);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn round_trip_initialize_list_and_call() {
    let dir = fixture_with_defect();
    let path = dir.path().join("bad.py");

    let (server_io, client_io) = tokio::io::duplex(8192);
    let (server_read, server_write) = tokio::io::split(server_io);
    let (client_read, client_write) = tokio::io::split(client_io);

    let server_task = tokio::spawn(async move {
        let service = PolyMcpServer::new(None)
            .serve((server_read, server_write))
            .await
            .unwrap();
        service.waiting().await.unwrap();
    });

    let client = ().serve((client_read, client_write)).await.unwrap();

    let tools = client.list_all_tools().await.unwrap();
    let names: Vec<String> = tools.iter().map(|t| t.name.to_string()).collect();
    assert!(names.contains(&"lint".to_string()));
    assert!(names.contains(&"lint_fix".to_string()));

    let mut arguments = serde_json::Map::new();
    arguments.insert("paths".into(), serde_json::json!([path.display().to_string()]));
    let result = client
        .call_tool(CallToolRequestParams::new("lint").with_arguments(arguments))
        .await
        .unwrap();

    let text = result.content[0].as_text().expect("text content").text.clone();
    let parsed: Value = serde_json::from_str(&text).unwrap();
    assert!(parsed.is_array(), "lint tool returns the CLI json array");

    client.cancel().await.unwrap();
    let _ = server_task.await;
}
