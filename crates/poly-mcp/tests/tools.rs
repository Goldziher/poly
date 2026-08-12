//! Tests for the `poly-mcp` server.
//!
//! Layers:
//! 1. `ops` behaviour — the synchronous engine calls produce the typed results.
//! 2. Tool-registry introspection — expected tool names and annotations.
//! 3. In-process round-trips over a tokio duplex transport: structured content +
//!    JSON/TOON text for the per-file tools and the new read-only tools, and the
//!    whole-project async **Task** lifecycle (`tools/call` → `tasks/get`).

use std::path::PathBuf;
use std::time::Duration;

use poly_mcp::identity::ExecutableWatch;
use poly_mcp::{PolyMcpServer, ops};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ClientCapabilities, ClientInfo, GetTaskParams,
    Implementation, TaskPayload, TaskStatus,
};
use rmcp::{ServerHandler, ServiceExt};
use serde_json::Value;

/// Write a Python file with a real lint defect (unused import → ruff F401) into
/// a temp dir.
fn fixture_with_defect() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("bad.py"), "import os\n").unwrap();
    dir
}

// ── Layer 1: ops behaviour ────────────────────────────────────────────────

#[test]
fn lint_results_carry_diagnostic_contract() {
    let dir = fixture_with_defect();
    let path = dir.path().join("bad.py");
    let results = ops::lint_results(&[path.display().to_string()], &[], None, false).unwrap();
    assert!(!results.is_empty(), "expected at least one lint result");
    let diagnostics = &results[0].diagnostics;
    assert!(!diagnostics.is_empty(), "the bad file has diagnostics");
    assert_eq!(diagnostics[0].engine, "ruff", "unused import is a ruff finding");
}

#[test]
fn format_results_report_changed_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.rs");
    let original = "fn main() {}   \n";
    std::fs::write(&path, original).unwrap();
    let results = ops::format_results(&[path.display().to_string()], &[], None, false).unwrap();
    assert_eq!(results.len(), 1, "one file scanned");
    assert!(results[0].changed, "trailing whitespace would be reformatted");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        original,
        "check mode does not write"
    );
}

#[test]
fn explicit_missing_config_is_an_error() {
    let result = ops::lint_results(&[".".to_string()], &[], Some("/nonexistent/poly.toml"), false);
    assert!(result.is_err(), "missing explicit config should error");
}

#[test]
fn cache_stats_report_has_format_version() {
    let report = ops::cache_stats().unwrap();
    assert!(!report.format_version.is_empty(), "format version is populated");
}

#[test]
fn config_show_reports_effective_defaults() {
    let report = ops::config_show(None).unwrap();
    // The opinionated default line length is 120 everywhere the tool exposes it.
    assert_eq!(report.defaults.line_length, 120, "opinionated default line length");
}

#[test]
fn rules_report_lists_configured_dirs() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("poly.toml"), "[rules]\ndirs = []\n").unwrap();
    let config = dir.path().join("poly.toml");
    let report = ops::rules_report(&[], Some(&config.display().to_string()), false).unwrap();
    assert!(report.dirs.is_empty(), "empty rule dirs from config");
    assert!(report.rules.is_empty(), "no rules discovered");
    assert!(report.tests.is_none(), "no test report when test=false");
}

// ── Layer 2: registry introspection ───────────────────────────────────────

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
            "config_show",
            "format_check",
            "format_write",
            "lint",
            "lint_fix",
            "rules",
            "version",
            "workspace_lint",
            "workspace_lint_fix",
        ]
    );

    for tool in [
        "lint",
        "format_check",
        "cache_stats",
        "rules",
        "config_show",
        "version",
        "workspace_lint",
    ] {
        let (read_only, destructive) = server.tool_hints(tool).unwrap();
        assert_eq!(read_only, Some(true), "{tool} should be read-only");
        assert_eq!(destructive, Some(false), "{tool} should not be destructive");
    }
    for tool in ["lint_fix", "format_write", "cache_clean", "workspace_lint_fix"] {
        let (read_only, destructive) = server.tool_hints(tool).unwrap();
        assert_eq!(read_only, Some(false), "{tool} should not be read-only");
        assert_eq!(destructive, Some(true), "{tool} should be destructive");
    }
}

#[test]
fn declared_output_schemas_include_the_identity_they_send() {
    let server = PolyMcpServer::new(None);
    for name in ["lint", "format_check", "cache_stats", "config_show", "version"] {
        let tool = server.get_tool(name).unwrap();
        let schema = tool
            .output_schema
            .as_ref()
            .unwrap_or_else(|| panic!("{name} declares an output schema"));
        let properties = schema["properties"].as_object().unwrap();
        assert!(
            properties.contains_key("poly"),
            "{name}'s schema declares the `poly` identity it actually sends"
        );
        let required = schema["required"].as_array().unwrap();
        assert!(
            required.contains(&Value::from("poly")),
            "{name}'s schema requires the identity, since every response carries it"
        );
    }
}

#[test]
fn server_constructs_with_config_override() {
    let server = PolyMcpServer::new(Some(PathBuf::from("poly.toml")));
    assert_eq!(server.tool_names().len(), 11);
}

// ── Layer 3: in-process round-trips ───────────────────────────────────────

/// Wire an in-process server to a duplex transport, returning the client and the
/// server's join handle. The client declares the tasks extension capability.
async fn connect() -> (
    rmcp::service::RunningService<rmcp::RoleClient, ClientInfo>,
    tokio::task::JoinHandle<()>,
) {
    let (server_io, client_io) = tokio::io::duplex(1 << 16);
    let (server_read, server_write) = tokio::io::split(server_io);
    let (client_read, client_write) = tokio::io::split(client_io);

    let server_task = tokio::spawn(async move {
        let service = PolyMcpServer::new(None)
            .serve((server_read, server_write))
            .await
            .unwrap();
        service.waiting().await.unwrap();
    });

    let client_info = ClientInfo::new(
        ClientCapabilities::builder().enable_tasks().build(),
        Implementation::new("poly-mcp-test", "0.0.0"),
    );
    let client = client_info.serve((client_read, client_write)).await.unwrap();
    (client, server_task)
}

fn arguments(pairs: &[(&str, Value)]) -> serde_json::Map<String, Value> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lint_returns_structured_content_and_json_text() {
    let dir = fixture_with_defect();
    let path = dir.path().join("bad.py");
    let (client, server_task) = connect().await;

    let names: Vec<String> = client
        .list_all_tools()
        .await
        .unwrap()
        .iter()
        .map(|t| t.name.to_string())
        .collect();
    assert!(names.contains(&"lint".to_string()));
    assert!(names.contains(&"workspace_lint".to_string()));

    let result = client
        .call_tool(
            CallToolRequestParams::new("lint")
                .with_arguments(arguments(&[("paths", Value::from(vec![path.display().to_string()]))])),
        )
        .await
        .unwrap();

    // Structured content is the typed LintReport object.
    let structured = result.structured_content.as_ref().expect("structured_content present");
    let results = structured["results"].as_array().expect("results array");
    assert_eq!(results.len(), 1, "one file linted");
    assert_eq!(results[0]["diagnostics"][0]["engine"], "ruff");

    // The default text block stays the CLI JSON array (backward-compatible).
    let text = result.content[0].as_text().expect("text content").text.clone();
    let parsed: Value = serde_json::from_str(&text).unwrap();
    assert!(parsed.is_array(), "json text block is the CLI array");

    client.cancel().await.unwrap();
    let _ = server_task.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lint_toon_text_differs_from_json_but_structured_matches() {
    let dir = fixture_with_defect();
    let path = dir.path().join("bad.py");
    let paths = Value::from(vec![path.display().to_string()]);
    let (client, server_task) = connect().await;

    let json_result = client
        .call_tool(CallToolRequestParams::new("lint").with_arguments(arguments(&[("paths", paths.clone())])))
        .await
        .unwrap();
    let toon_result = client
        .call_tool(
            CallToolRequestParams::new("lint")
                .with_arguments(arguments(&[("paths", paths), ("format", Value::from("toon"))])),
        )
        .await
        .unwrap();

    let json_text = json_result.content[0].as_text().unwrap().text.clone();
    let toon_text = toon_result.content[0].as_text().unwrap().text.clone();
    assert_ne!(json_text, toon_text, "toon text differs from json text");
    assert!(
        serde_json::from_str::<Value>(&toon_text).is_err(),
        "the toon text is not JSON: {toon_text}"
    );
    // structured_content is JSON regardless of the requested text representation.
    assert_eq!(
        json_result.structured_content, toon_result.structured_content,
        "structured content is JSON in both cases"
    );

    client.cancel().await.unwrap();
    let _ = server_task.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cache_stats_tool_returns_typed_structured_content() {
    let (client, server_task) = connect().await;
    let result = client
        .call_tool(CallToolRequestParams::new("cache_stats"))
        .await
        .unwrap();
    let structured = result.structured_content.as_ref().expect("structured_content present");
    assert!(structured["format_version"].is_string(), "typed format_version");
    assert!(structured["per_namespace"].is_array(), "typed per_namespace");
    client.cancel().await.unwrap();
    let _ = server_task.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_show_tool_returns_effective_defaults() {
    let (client, server_task) = connect().await;
    let result = client
        .call_tool(CallToolRequestParams::new("config_show"))
        .await
        .unwrap();
    let structured = result.structured_content.as_ref().expect("structured_content present");
    assert_eq!(
        structured["defaults"]["line_length"], 120,
        "opinionated default line length"
    );
    client.cancel().await.unwrap();
    let _ = server_task.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rules_tool_lists_rules_structured() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("poly.toml"), "[rules]\ndirs = []\n").unwrap();
    let config = dir.path().join("poly.toml").display().to_string();
    let (client, server_task) = connect().await;
    let result = client
        .call_tool(CallToolRequestParams::new("rules").with_arguments(arguments(&[("config", Value::from(config))])))
        .await
        .unwrap();
    let structured = result.structured_content.as_ref().expect("structured_content present");
    assert_eq!(structured["rules"], serde_json::json!([]), "no rules discovered");
    client.cancel().await.unwrap();
    let _ = server_task.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_lint_runs_as_async_task_and_completes() {
    // A config that disables the whole-project phase makes the Task settle
    // immediately (no cargo invocation), so the test is fast and deterministic.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("poly.toml"), "[lint]\nworkspace = false\n").unwrap();
    let config = dir.path().join("poly.toml").display().to_string();
    let (client, server_task) = connect().await;

    // The whole-project tool returns a task handle, not a completed result.
    let response = client
        .call_tool_once(
            CallToolRequestParams::new("workspace_lint").with_arguments(arguments(&[("config", Value::from(config))])),
        )
        .await
        .unwrap();
    let create = match response {
        CallToolResponse::Task(create) => create,
        other => panic!("expected a task handle, got {other:?}"),
    };
    assert_eq!(create.task.status, TaskStatus::Working, "seed task is working");
    let task_id = create.task.task_id.clone();

    // Poll tasks/get until the task settles.
    let mut settled = None;
    for _ in 0..200 {
        let detailed = client.get_task(GetTaskParams::new(task_id.clone())).await.unwrap();
        if detailed.task.status().is_terminal() {
            settled = Some(detailed);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let settled = settled.expect("task settled");
    assert_eq!(settled.task.status(), TaskStatus::Completed, "task completed");

    let TaskPayload::Completed { result } = settled.task.payload else {
        panic!("expected a completed payload");
    };
    let call_result: CallToolResult = serde_json::from_value(Value::Object(result)).unwrap();
    let structured = call_result
        .structured_content
        .expect("structured content in task result");
    assert_eq!(structured["passed"], Value::Bool(true), "disabled phase passes");
    assert_eq!(structured["tools"], serde_json::json!([]), "no tools ran");

    client.cancel().await.unwrap();
    let _ = server_task.await;
}

// ── Layer 4: the serving binary's identity ────────────────────────────────

/// Assert a tool result carries the `poly` identity in both places a caller
/// might look: `structured_content` and the response `_meta`.
fn assert_identifies_the_binary(result: &CallToolResult, tool: &str) {
    let structured = result
        .structured_content
        .as_ref()
        .unwrap_or_else(|| panic!("{tool} returns structured content"));
    let identity = &structured["poly"];
    assert_eq!(
        identity["version"],
        Value::from(poly_buildinfo::VERSION),
        "{tool} names the serving version"
    );
    assert_eq!(
        identity["build_id"],
        Value::from(poly_buildinfo::build_id()),
        "{tool} names the build id, so a dev build cannot pass for a release"
    );
    assert_eq!(
        identity["channel"],
        Value::from(poly_buildinfo::channel().as_str()),
        "{tool} names the channel"
    );
    assert_eq!(
        identity["pid"],
        Value::from(std::process::id()),
        "{tool} names the serving process"
    );

    let meta = result.meta.as_ref().unwrap_or_else(|| panic!("{tool} sets _meta"));
    assert_eq!(
        meta.0.get("poly").map(|value| &value["version"]),
        Some(&Value::from(poly_buildinfo::VERSION)),
        "{tool} repeats the identity in _meta"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_tool_response_identifies_the_binary_that_answered() {
    let dir = fixture_with_defect();
    let path = dir.path().join("bad.py");
    let (client, server_task) = connect().await;

    // One read-only tool of each shape: per-file, parameterless, config-driven,
    // and the identity tool itself.
    let lint = client
        .call_tool(
            CallToolRequestParams::new("lint")
                .with_arguments(arguments(&[("paths", Value::from(vec![path.display().to_string()]))])),
        )
        .await
        .unwrap();
    assert_identifies_the_binary(&lint, "lint");

    for tool in ["format_check", "cache_stats", "config_show", "version"] {
        let result = client.call_tool(CallToolRequestParams::new(tool)).await.unwrap();
        assert_identifies_the_binary(&result, tool);
    }

    client.cancel().await.unwrap();
    let _ = server_task.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn version_tool_reports_executable_health_and_uptime() {
    let (client, server_task) = connect().await;
    let result = client.call_tool(CallToolRequestParams::new("version")).await.unwrap();
    let structured = result.structured_content.as_ref().expect("structured content");
    assert_eq!(
        structured["executable_current"],
        Value::Bool(true),
        "the test binary has not been replaced mid-run"
    );
    assert!(
        structured["uptime_seconds"].is_u64(),
        "uptime lets a caller tell a pre-upgrade server from a fresh one"
    );
    client.cancel().await.unwrap();
    let _ = server_task.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn adding_identity_does_not_disturb_the_existing_payload() {
    let dir = fixture_with_defect();
    let path = dir.path().join("bad.py");
    let (client, server_task) = connect().await;
    let result = client
        .call_tool(
            CallToolRequestParams::new("lint")
                .with_arguments(arguments(&[("paths", Value::from(vec![path.display().to_string()]))])),
        )
        .await
        .unwrap();

    // The `results` array and the CLI-identical text block are unchanged; the
    // identity is purely additive.
    let structured = result.structured_content.as_ref().unwrap();
    assert_eq!(structured["results"].as_array().unwrap().len(), 1);
    let text = result.content[0].as_text().unwrap().text.clone();
    assert!(
        serde_json::from_str::<Value>(&text).unwrap().is_array(),
        "the text block is still the CLI array"
    );

    client.cancel().await.unwrap();
    let _ = server_task.await;
}

/// Wire an in-process server whose executable watch points at `executable`,
/// so a test can replace or delete that file mid-session.
async fn connect_watching(
    executable: PathBuf,
) -> (
    rmcp::service::RunningService<rmcp::RoleClient, ClientInfo>,
    tokio::task::JoinHandle<()>,
) {
    let (server_io, client_io) = tokio::io::duplex(1 << 16);
    let (server_read, server_write) = tokio::io::split(server_io);
    let (client_read, client_write) = tokio::io::split(client_io);

    let server_task = tokio::spawn(async move {
        let watch = ExecutableWatch::over(Some(executable));
        let service = PolyMcpServer::with_executable_watch(None, watch)
            .serve((server_read, server_write))
            .await
            .unwrap();
        let _ = service.waiting().await;
    });

    let client_info = ClientInfo::new(
        ClientCapabilities::builder().enable_tasks().build(),
        Implementation::new("poly-mcp-test", "0.0.0"),
    );
    let client = client_info.serve((client_read, client_write)).await.unwrap();
    (client, server_task)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_server_whose_binary_was_replaced_refuses_to_answer() {
    let dir = tempfile::tempdir().unwrap();
    let executable = dir.path().join("poly");
    std::fs::write(&executable, b"the build this server started from").unwrap();
    let (client, server_task) = connect_watching(executable.clone()).await;

    // While it is still the same file, tools answer normally.
    assert!(
        client
            .call_tool(CallToolRequestParams::new("cache_stats"))
            .await
            .is_ok(),
        "an untouched executable serves normally"
    );

    // The upgrade: same path, different file. The running server holds the old
    // inode and would otherwise keep answering from it forever.
    std::fs::remove_file(&executable).unwrap();
    std::fs::write(&executable, b"a newer build installed underneath us").unwrap();

    let error = client
        .call_tool(CallToolRequestParams::new("cache_stats"))
        .await
        .expect_err("a replaced executable must fail, not answer");
    let message = error.to_string();
    assert!(
        message.contains("replaced on disk"),
        "the error says what happened: {message}"
    );
    assert!(
        message.contains("Restart the MCP server"),
        "the error says what to do: {message}"
    );

    client.cancel().await.unwrap();
    let _ = server_task.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_version_tool_still_answers_after_the_binary_is_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let executable = dir.path().join("poly");
    std::fs::write(&executable, b"the build this server started from").unwrap();
    let (client, server_task) = connect_watching(executable.clone()).await;

    std::fs::remove_file(&executable).unwrap();

    // Everything else is refused, but `version` must still diagnose the cause —
    // an error with no explanation leaves the caller exactly where they started.
    assert!(
        client.call_tool(CallToolRequestParams::new("lint")).await.is_err(),
        "lint is refused once the binary is gone"
    );
    let result = client
        .call_tool(CallToolRequestParams::new("version"))
        .await
        .expect("version answers even when the binary is gone");
    let structured = result.structured_content.as_ref().expect("structured content");
    assert_eq!(
        structured["executable_current"],
        Value::Bool(false),
        "version reports the executable as stale"
    );
    assert!(
        structured["executable_status"]
            .as_str()
            .is_some_and(|status| status.contains("deleted from disk")),
        "version explains why the others refused: {structured}"
    );

    client.cancel().await.unwrap();
    let _ = server_task.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_task_for_unknown_id_is_an_error() {
    let (client, server_task) = connect().await;
    let result = client.get_task(GetTaskParams::new("does-not-exist")).await;
    assert!(result.is_err(), "unknown task id is rejected");
    client.cancel().await.unwrap();
    let _ = server_task.await;
}
