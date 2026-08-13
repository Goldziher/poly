//! Coverage for files an engine could not process, seen through the MCP tools.
//!
//! `poly_core::lint()` / `format()` return a bare `Vec` of per-file results and
//! drop the per-file failures the run recorded, so a file poly *failed* on simply
//! vanished from the payload. On the CLI that still surfaced as exit code 2; an
//! MCP caller has no exit code, so the tool answered with a clean-looking result
//! for a file it had never read. These tests pin the three outcomes apart:
//!
//! - **clean** — checked, nothing to report;
//! - **skipped** — poly correctly declined the file (no engine covers it);
//! - **errored** — poly failed on a file it accepted, so the file is *not*
//!   verified and the tool result is not a success.
//!
//! The failure is induced with a `.py` file holding invalid UTF-8, matching the
//! CLI-side precedent (`crates/poly-cli/tests/lint_errors.rs`): the runner reads
//! every file as text before any engine sees it, so the error is deterministic,
//! parallel-safe, and independent of the host toolchain.

use std::path::Path;

use poly_mcp::dto::{FormatReport, LintReport};
use poly_mcp::{PolyMcpServer, ops};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ClientCapabilities, ClientInfo};
use serde_json::Value;
use tempfile::TempDir;

/// Bytes that are not valid UTF-8, in a file poly does route to an engine.
const INVALID_UTF8: &[u8] = b"x = 1\n\xff\xfe not utf-8\n";

/// A `.csproj` is routed nowhere: no poly engine claims the extension, so naming
/// it is a *skip*, not an error.
const CSPROJ: &str = "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n  </PropertyGroup>\n</Project>\n";

/// The message `std::fs::read_to_string` fails with on non-UTF-8 bytes, which is
/// what the runner flattens into the per-file error.
const UTF8_ERROR: &str = "stream did not contain valid UTF-8";

/// The reason recorded for an explicitly named path no backend covers.
const NO_ENGINE: &str = "no matching engine for this file type";

/// A repo with one file poly handles cleanly, one it cannot read, and one no
/// engine covers — the three outcomes that must stay distinguishable.
fn repo() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("ok.py"), "print(\"hi\")\n").expect("write ok.py");
    std::fs::write(dir.path().join("bad.py"), INVALID_UTF8).expect("write bad.py");
    std::fs::write(dir.path().join("App.csproj"), CSPROJ).expect("write App.csproj");
    dir
}

/// The three paths, in the order the tools receive them.
fn paths(dir: &TempDir) -> Vec<String> {
    ["ok.py", "bad.py", "App.csproj"]
        .iter()
        .map(|name| dir.path().join(name).display().to_string())
        .collect()
}

/// The single entry in `results` whose `path` ends with `name`.
fn entry<'a>(results: &'a [Value], name: &str) -> &'a Value {
    let matches: Vec<&Value> = results
        .iter()
        .filter(|entry| {
            entry["path"]
                .as_str()
                .is_some_and(|path| Path::new(path).file_name().is_some_and(|file| file == name))
        })
        .collect();
    assert_eq!(matches.len(), 1, "exactly one `{name}` entry in {results:?}");
    matches[0]
}

// ── ops layer: the run-level accounting reaches the caller ────────────────

#[test]
fn lint_run_carries_the_errored_file_instead_of_dropping_it() {
    let dir = repo();
    let run = ops::lint_run(&paths(&dir), &[], None, false).unwrap();

    assert_eq!(run.errors.len(), 1, "the unreadable file is reported, not dropped");
    assert_eq!(run.errors[0].path, dir.path().join("bad.py"));
    assert_eq!(run.errors[0].message, UTF8_ERROR);
    assert_eq!(
        run.skipped.len(),
        1,
        "the errored file must not be counted as a skip: {:?}",
        run.skipped
    );
    assert_eq!(run.skipped[0].path, dir.path().join("App.csproj"));
    assert_eq!(run.skipped[0].reason, NO_ENGINE);
    assert_eq!(run.checked, 1, "only the readable file was linted");
}

#[test]
fn format_run_carries_the_errored_file_instead_of_dropping_it() {
    let dir = repo();
    let run = ops::format_run(&paths(&dir), &[], None, false).unwrap();

    assert_eq!(run.errors.len(), 1, "the unreadable file is reported, not dropped");
    assert_eq!(run.errors[0].path, dir.path().join("bad.py"));
    assert_eq!(run.errors[0].message, UTF8_ERROR);
    assert_eq!(
        run.skipped.len(),
        1,
        "the errored file must not be counted as a skip: {:?}",
        run.skipped
    );
    assert_eq!(run.skipped[0].path, dir.path().join("App.csproj"));
    assert_eq!(run.skipped[0].reason, NO_ENGINE);
}

// ── CLI parity: the per-file records are the CLI's ────────────────────────

/// The MCP lint records must stay the CLI's, or the documented "mirrors the
/// CLI" contract quietly stops being true — this is the drift guard on the
/// synthetic error/skip entries, which both sides build independently.
#[test]
fn the_lint_records_are_the_cli_records() {
    let dir = repo();
    let run = ops::lint_run(&paths(&dir), &[], None, false).unwrap();
    let cli: Value = serde_json::from_str(&poly_core::report::report_lint_json_run(&run)).expect("CLI JSON");
    let mcp = serde_json::to_value(LintReport::from_run(run).results).expect("MCP JSON");
    assert_eq!(mcp, cli, "the MCP lint records are exactly the CLI's");
}

/// Same for format. The CLI's format document used to have no `error` field and
/// dropped the errored files entirely — they showed up only in its exit code —
/// so the MCP side carried them alone. `FormatResult::error` closed that gap, and
/// the two sides are now the same records, errored files included.
#[test]
fn the_format_records_are_the_cli_records() {
    let dir = repo();
    let run = ops::format_run(&paths(&dir), &[], None, false).unwrap();
    let cli: Value = serde_json::from_str(&poly_core::report::report_format_json_run(&run)).expect("CLI JSON");
    let report = FormatReport::from_run(run);
    assert_eq!(
        report.results.iter().filter(|r| r.error.is_some()).count(),
        1,
        "the errored file is one of the records, not an omission"
    );
    let mcp = serde_json::to_value(&report.results).expect("MCP JSON");
    assert_eq!(mcp, cli, "the MCP format records are exactly the CLI's");
}

// ── round-trips: the serialized MCP payload ───────────────────────────────

/// Wire an in-process server to a duplex transport, returning the client and the
/// server's join handle.
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
        ClientCapabilities::builder().build(),
        rmcp::model::Implementation::new("poly-mcp-test", "0.0.0"),
    );
    let client = client_info.serve((client_read, client_write)).await.unwrap();
    (client, server_task)
}

/// Call `tool` over the three fixture paths and return the raw result.
async fn call(tool: &str, dir: &TempDir) -> rmcp::model::CallToolResult {
    let (client, server_task) = connect().await;
    let arguments: serde_json::Map<String, Value> =
        [("paths".to_string(), Value::from(paths(dir)))].into_iter().collect();
    let result = client
        .call_tool(CallToolRequestParams::new(tool.to_string()).with_arguments(arguments))
        .await
        .unwrap();
    client.cancel().await.unwrap();
    let _ = server_task.await;
    result
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lint_names_the_errored_file_and_does_not_report_success() {
    let dir = repo();
    let result = call("lint", &dir).await;

    assert_eq!(
        result.is_error,
        Some(true),
        "a file poly failed on must not present as a successful tool call"
    );

    let structured = result.structured_content.as_ref().expect("structured content");
    let results = structured["results"].as_array().expect("results array");

    let errored = entry(results, "bad.py");
    assert_eq!(
        errored["error"],
        Value::from(UTF8_ERROR),
        "the error is machine-readable"
    );
    assert_eq!(errored.get("skipped"), None, "an errored file is not a skipped one");
    assert_eq!(
        errored["diagnostics"],
        Value::Array(vec![]),
        "a file that could not be read has no findings"
    );

    let skipped = entry(results, "App.csproj");
    assert_eq!(skipped["skipped"], Value::from(NO_ENGINE));
    assert_eq!(skipped.get("error"), None, "a skip is not an error");

    let errors = structured["errors"].as_array().expect("errors array");
    assert_eq!(errors.len(), 1, "the run-level error list names the file: {errors:?}");
    assert_eq!(errors[0]["message"], Value::from(UTF8_ERROR));
    assert!(
        errors[0]["path"].as_str().is_some_and(|path| path.ends_with("bad.py")),
        "the run-level error names the path: {errors:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn format_check_names_the_errored_file_and_does_not_report_success() {
    let dir = repo();
    let result = call("format_check", &dir).await;

    assert_eq!(
        result.is_error,
        Some(true),
        "a file poly failed on must not present as a successful tool call"
    );

    let structured = result.structured_content.as_ref().expect("structured content");
    let results = structured["results"].as_array().expect("results array");

    let errored = entry(results, "bad.py");
    assert_eq!(
        errored["error"],
        Value::from(UTF8_ERROR),
        "the error is machine-readable"
    );
    assert_eq!(errored.get("skipped"), None, "an errored file is not a skipped one");
    assert_eq!(
        errored["changed"],
        Value::Bool(false),
        "a file that could not be read was not reformatted"
    );

    let skipped = entry(results, "App.csproj");
    assert_eq!(skipped["skipped"], Value::from(NO_ENGINE));
    assert_eq!(skipped.get("error"), None, "a skip is not an error");

    let clean = entry(results, "ok.py");
    assert_eq!(clean.get("error"), None, "the clean file carries no error");
    assert_eq!(clean.get("skipped"), None, "the clean file was not skipped");

    let errors = structured["errors"].as_array().expect("errors array");
    assert_eq!(errors.len(), 1, "the run-level error list names the file: {errors:?}");
    assert_eq!(errors[0]["message"], Value::from(UTF8_ERROR));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_text_block_carries_the_errored_file_too() {
    let dir = repo();
    for tool in ["lint", "format_check"] {
        let result = call(tool, &dir).await;
        let text = result.content[0].as_text().expect("text content").text.clone();
        let parsed: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{tool} text is JSON ({e}): {text}"));
        let records = parsed
            .as_array()
            .unwrap_or_else(|| panic!("{tool} text block stays the CLI array: {text}"));
        assert_eq!(
            entry(records, "bad.py")["error"],
            Value::from(UTF8_ERROR),
            "{tool}'s text block must not hide the failure: {text}"
        );
        let structured = result.structured_content.as_ref().expect("structured content");
        assert_eq!(
            &parsed, &structured["results"],
            "{tool}'s text block and structured results are the same records"
        );
    }
}

// ── the clean path is unchanged ───────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_clean_run_gains_no_error_fields() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("ok.py"), "print(\"hi\")\n").expect("write ok.py");
    let only = vec![dir.path().join("ok.py").display().to_string()];

    for tool in ["lint", "format_check"] {
        let (client, server_task) = connect().await;
        let arguments: serde_json::Map<String, Value> =
            [("paths".to_string(), Value::from(only.clone()))].into_iter().collect();
        let result = client
            .call_tool(CallToolRequestParams::new(tool.to_string()).with_arguments(arguments))
            .await
            .unwrap();
        client.cancel().await.unwrap();
        let _ = server_task.await;

        assert_eq!(result.is_error, Some(false), "{tool}: a clean run is a success");
        let structured = result.structured_content.as_ref().expect("structured content");
        assert_eq!(
            structured["errors"],
            Value::Array(vec![]),
            "{tool}: a clean run has no errors"
        );
        for record in structured["results"].as_array().expect("results array") {
            assert_eq!(record.get("error"), None, "{tool}: no spurious error field: {record}");
            assert_eq!(record.get("skipped"), None, "{tool}: no spurious skip field: {record}");
        }
    }
}
