//! The component tools and the task-optional run tools, over a fake component host.
//!
//! The host here is a [`ComponentsProvider`] with fixed answers that counts what it is asked,
//! behind the real host services (so the lint pipeline is the real one). Nothing downloads: what
//! a Task-or-block decision rests on is the provider's `would_download` and the install plan.
//!
//! Every server in this binary uses the same provider, because the identity block is process-wide.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use poly_frontend_api::dto::{ComponentAction, ComponentActions, ComponentSummary, ComponentsStatus};
use poly_frontend_api::wire::{
    ComponentsInstallRequest, ComponentsStatusRequest, ComponentsUninstallRequest, InstallMode,
};
use poly_host_services::ComponentsProvider;
use rmcp::ServiceExt;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ClientCapabilities, ClientConfig, GetTaskParams,
    Implementation, TaskPayload, TaskStatus,
};
use serde_json::Value;

/// A component host that would download `download` and nothing else.
struct Fake {
    download: Vec<String>,
    statuses: AtomicUsize,
    installs: AtomicUsize,
}

impl Fake {
    fn new(download: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            download: download.iter().map(|name| (*name).to_owned()).collect(),
            statuses: AtomicUsize::new(0),
            installs: AtomicUsize::new(0),
        })
    }
}

fn row(name: &str, action: &str, detail: Option<&str>) -> ComponentAction {
    ComponentAction {
        name: name.to_owned(),
        action: action.to_owned(),
        detail: detail.map(str::to_owned),
    }
}

fn actions(command: &str, results: Vec<ComponentAction>) -> ComponentActions {
    ComponentActions {
        command: command.to_owned(),
        offline: false,
        ok: results.iter().all(|r| r.action != "failed" && r.action != "refused"),
        results,
        freed_bytes: None,
        notes: Vec::new(),
    }
}

impl ComponentsProvider for Fake {
    fn components(&self) -> anyhow::Result<Vec<ComponentSummary>> {
        Ok(Vec::new())
    }

    fn identity(&self) -> (String, Option<String>) {
        ("core".to_owned(), Some("deadbeef".to_owned()))
    }

    fn status(&self, request: &ComponentsStatusRequest) -> anyhow::Result<ComponentsStatus> {
        self.statuses.fetch_add(1, Ordering::SeqCst);
        Ok(ComponentsStatus {
            flavor: "core".to_owned(),
            offline: false,
            cache_root: "/nowhere".to_owned(),
            components_lock: Some("deadbeef".to_owned()),
            lock_error: None,
            components: Vec::new(),
            missing: Some(self.download.clone()),
            would_download: if request.install == InstallMode::Never {
                Vec::new()
            } else {
                self.download.clone()
            },
        })
    }

    fn install(&self, request: &ComponentsInstallRequest) -> anyhow::Result<ComponentActions> {
        let rows = request
            .names
            .iter()
            .map(|name| {
                let needs = self.download.contains(name);
                match (request.dry_run, needs) {
                    (true, true) => row(name, "would_install", None),
                    (false, true) => row(name, "installed", None),
                    (_, false) => row(name, "already_installed", None),
                }
            })
            .collect();
        if !request.dry_run {
            self.installs.fetch_add(1, Ordering::SeqCst);
        }
        Ok(actions("install", rows))
    }

    fn uninstall(&self, request: &ComponentsUninstallRequest) -> anyhow::Result<ComponentActions> {
        let rows = request
            .names
            .iter()
            .map(|name| {
                if name == "mcp" {
                    row(
                        name,
                        "refused",
                        Some("loaded in this process; restart poly to remove it"),
                    )
                } else {
                    row(name, "removed", None)
                }
            })
            .collect();
        Ok(actions("uninstall", rows))
    }
}

type Client = rmcp::service::RunningService<rmcp::RoleClient, ClientConfig>;

async fn connect(provider: Arc<Fake>, tasks: bool) -> (Client, tokio::task::JoinHandle<()>) {
    let (server_io, client_io) = tokio::io::duplex(1 << 16);
    let (server_read, server_write) = tokio::io::split(server_io);
    let (client_read, client_write) = tokio::io::split(client_io);
    let server_task = tokio::spawn(async move {
        let service = common::server_with_components(provider)
            .serve((server_read, server_write))
            .await
            .unwrap();
        service.waiting().await.unwrap();
    });
    let capabilities = if tasks {
        ClientCapabilities::builder().enable_tasks().build()
    } else {
        ClientCapabilities::builder().build()
    };
    let client = ClientConfig::new(capabilities, Implementation::new("poly-mcp-test", "0.0.0"))
        .serve((client_read, client_write))
        .await
        .unwrap();
    (client, server_task)
}

fn arguments(pairs: &[(&str, Value)]) -> serde_json::Map<String, Value> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect()
}

fn names(list: &[&str]) -> Value {
    Value::from(list.iter().map(|name| (*name).to_owned()).collect::<Vec<_>>())
}

/// Call a tool and expect a Task handle back; returns its id and initial status message.
async fn expect_task(client: &Client, tool: &str, args: serde_json::Map<String, Value>) -> (String, String) {
    let response = client
        .call_tool_once(CallToolRequestParams::new(tool.to_owned()).with_arguments(args))
        .await
        .unwrap();
    match response {
        CallToolResponse::Task(create) => (
            create.task.task_id.clone(),
            create.task.status_message.clone().unwrap_or_default(),
        ),
        other => panic!("{tool}: expected a task handle, got {other:?}"),
    }
}

/// Call a tool and expect a completed result, never a Task.
async fn expect_complete(client: &Client, tool: &str, args: serde_json::Map<String, Value>) -> CallToolResult {
    let response = client
        .call_tool_once(CallToolRequestParams::new(tool.to_owned()).with_arguments(args))
        .await
        .unwrap();
    match response {
        CallToolResponse::Complete(result) => result,
        other => panic!("{tool}: expected a completed result, got {other:?}"),
    }
}

async fn settle(client: &Client, task_id: &str) -> CallToolResult {
    for _ in 0..300 {
        let detailed = client.get_task(GetTaskParams::new(task_id.to_owned())).await.unwrap();
        if detailed.task.status().is_terminal() {
            assert_eq!(detailed.task.status(), TaskStatus::Completed, "the task completed");
            let TaskPayload::Completed { result } = detailed.task.payload else {
                panic!("expected a completed payload");
            };
            return serde_json::from_value(Value::Object(result)).unwrap();
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the task did not settle");
}

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("bad.py"), "import os\n").unwrap();
    dir
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn components_reports_what_a_run_would_download_and_downloads_nothing() {
    let fake = Fake::new(&["web"]);
    let (client, server_task) = connect(Arc::clone(&fake), true).await;
    let result = expect_complete(&client, "components", arguments(&[("paths", names(&["."]))])).await;
    let structured = result.structured_content.as_ref().unwrap();
    assert_eq!(structured["would_download"], serde_json::json!(["web"]));
    assert_eq!(structured["missing"], serde_json::json!(["web"]));
    assert_eq!(fake.installs.load(Ordering::SeqCst), 0, "status never installs");

    let identity = &structured["poly"];
    assert_eq!(identity["flavor"], "core");
    assert_eq!(identity["components_lock"], "deadbeef");

    let never = expect_complete(
        &client,
        "components",
        arguments(&[("paths", names(&["."])), ("install", Value::from("never"))]),
    )
    .await;
    assert_eq!(
        never.structured_content.as_ref().unwrap()["would_download"],
        serde_json::json!([]),
        "install = never downloads nothing"
    );
    client.cancel().await.unwrap();
    let _ = server_task.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_install_that_downloads_is_a_task_naming_the_components() {
    let fake = Fake::new(&["web"]);
    let (client, server_task) = connect(Arc::clone(&fake), true).await;
    let (task_id, message) = expect_task(&client, "components_install", arguments(&[("names", names(&["web"]))])).await;
    assert!(
        message.contains("web"),
        "the status says what is downloading: {message}"
    );
    let result = settle(&client, &task_id).await;
    let structured = result.structured_content.unwrap();
    assert_eq!(structured["results"][0]["action"], "installed");
    assert_eq!(fake.installs.load(Ordering::SeqCst), 1);
    client.cancel().await.unwrap();
    let _ = server_task.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_install_with_nothing_to_download_answers_inline_without_installing() {
    let fake = Fake::new(&["web"]);
    let (client, server_task) = connect(Arc::clone(&fake), true).await;
    let result = expect_complete(
        &client,
        "components_install",
        arguments(&[("names", names(&["python"]))]),
    )
    .await;
    assert_eq!(
        result.structured_content.unwrap()["results"][0]["action"],
        "already_installed"
    );
    assert_eq!(fake.installs.load(Ordering::SeqCst), 0);
    client.cancel().await.unwrap();
    let _ = server_task.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dry_run_install_reports_the_plan_and_downloads_nothing() {
    let fake = Fake::new(&["web"]);
    let (client, server_task) = connect(Arc::clone(&fake), true).await;
    let result = expect_complete(
        &client,
        "components_install",
        arguments(&[("names", names(&["web"])), ("dry_run", Value::from(true))]),
    )
    .await;
    assert_eq!(
        result.structured_content.unwrap()["results"][0]["action"],
        "would_install"
    );
    assert_eq!(fake.installs.load(Ordering::SeqCst), 0);
    client.cancel().await.unwrap();
    let _ = server_task.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_without_tasks_gets_the_install_inline() {
    let fake = Fake::new(&["web"]);
    let (client, server_task) = connect(Arc::clone(&fake), false).await;
    let result = expect_complete(&client, "components_install", arguments(&[("names", names(&["web"]))])).await;
    assert_eq!(result.structured_content.unwrap()["results"][0]["action"], "installed");
    assert_eq!(fake.installs.load(Ordering::SeqCst), 1);
    client.cancel().await.unwrap();
    let _ = server_task.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lint_that_would_download_is_a_task_and_a_never_policy_is_not() {
    let dir = fixture();
    let path = dir.path().join("bad.py").display().to_string();
    let fake = Fake::new(&["web"]);
    let (client, server_task) = connect(Arc::clone(&fake), true).await;

    let (task_id, message) = expect_task(&client, "lint", arguments(&[("paths", names(&[&path]))])).await;
    assert!(message.contains("web"), "{message}");
    let result = settle(&client, &task_id).await;
    let structured = result.structured_content.unwrap();
    assert!(
        structured["summary"].is_object(),
        "the task result is the lint document: {structured}"
    );
    assert!(structured["poly"].is_object(), "and it carries the identity");

    let probes = fake.statuses.load(Ordering::SeqCst);
    let inline = expect_complete(
        &client,
        "lint",
        arguments(&[("paths", names(&[&path])), ("install", Value::from("never"))]),
    )
    .await;
    assert!(inline.structured_content.unwrap()["summary"].is_object());
    assert_eq!(
        fake.statuses.load(Ordering::SeqCst),
        probes,
        "install = never needs no probe: nothing can be downloaded"
    );
    client.cancel().await.unwrap();
    let _ = server_task.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lint_with_nothing_to_download_stays_inline() {
    let dir = fixture();
    let path = dir.path().join("bad.py").display().to_string();
    let (client, server_task) = connect(Fake::new(&[]), true).await;
    let result = expect_complete(&client, "format_check", arguments(&[("paths", names(&[&path]))])).await;
    assert!(result.structured_content.unwrap()["summary"].is_object());
    client.cancel().await.unwrap();
    let _ = server_task.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uninstall_refuses_a_loaded_component_and_marks_the_call_an_error() {
    let (client, server_task) = connect(Fake::new(&[]), true).await;

    let refused = expect_complete(
        &client,
        "components_uninstall",
        arguments(&[("names", names(&["mcp"]))]),
    )
    .await;
    assert_eq!(refused.is_error, Some(true), "a refusal is a tool error");
    assert_eq!(refused.structured_content.unwrap()["results"][0]["action"], "refused");

    let removed = expect_complete(
        &client,
        "components_uninstall",
        arguments(&[("names", names(&["web"]))]),
    )
    .await;
    assert_eq!(removed.is_error, Some(false));
    assert_eq!(removed.structured_content.unwrap()["results"][0]["action"], "removed");
    client.cancel().await.unwrap();
    let _ = server_task.await;
}
