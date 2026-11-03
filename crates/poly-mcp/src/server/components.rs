//! The lint, format and component tools that may download, registered by hand.
//!
//! See "Component downloads" in the docs of [`super`]: the four path-oriented tools and
//! `components_install` share one Task-or-block flow.

use std::sync::Arc;

use rmcp::model::{CallToolRequestParams, CallToolResponse, CallToolResult, JsonObject, Tool, ToolAnnotations};
use rmcp::service::RequestContext;
use rmcp::task_manager::{TaskExit, TaskOptions};
use rmcp::{ErrorData, RoleServer};
use schemars::JsonSchema;
use serde::Deserialize;

use poly_frontend_api::wire::{
    ComponentsInstallRequest, ComponentsStatusRequest, InstallMode, PathsRequest as PathsCall, RunReply,
};

use super::{PathsParams, PolyMcpServer, effective_config, parse_arguments, paths_call, run_blocking};
use crate::backend::Backend;
use crate::dto::{self, ComponentActions, TextRepr, actions_result};

/// Tool name for the check-mode lint.
const LINT: &str = "lint";
/// Tool name for the autofixing lint.
const LINT_FIX: &str = "lint_fix";
/// Tool name for the dry-run format.
const FORMAT_CHECK: &str = "format_check";
/// Tool name for the writing format.
const FORMAT_WRITE: &str = "format_write";
/// Tool name for the component installer, a Task when it downloads.
pub(super) const COMPONENTS_INSTALL: &str = "components_install";

/// Arguments for `components`.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct ComponentsParams {
    /// Files or directories to judge. With paths, the result lists only the components they need
    /// and says which are missing and which a run would download; without, every component.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Optional path to a config file (`poly.toml`).
    #[serde(default)]
    pub config: Option<String>,
    /// The install policy `would_download` is judged under; see `lint`'s `install`.
    #[serde(default)]
    pub install: InstallMode,
    /// Text representation for the result's text content block.
    #[serde(default)]
    pub format: TextRepr,
}

/// Arguments for `components_install`.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct ComponentsInstallParams {
    /// Component names or presets (`all`, a family name, a language alias such as `python`).
    #[serde(default)]
    pub names: Vec<String>,
    /// Languages whose components to install.
    #[serde(default)]
    pub languages: Vec<String>,
    /// Files or directories; installs every component they need.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Optional path to a config file (`poly.toml`), used to work out what `paths` need.
    #[serde(default)]
    pub config: Option<String>,
    /// Report what would be downloaded and download nothing.
    #[serde(default)]
    pub dry_run: bool,
    /// Text representation for the result's text content block.
    #[serde(default)]
    pub format: TextRepr,
}

/// Arguments for `components_uninstall`.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct ComponentsUninstallParams {
    /// Component names or presets to remove.
    #[serde(default)]
    pub names: Vec<String>,
    /// Also remove cached archives the lock no longer references and nothing used recently.
    #[serde(default)]
    pub stale: bool,
    /// Text representation for the result's text content block.
    #[serde(default)]
    pub format: TextRepr,
}

/// The four path-oriented tools, which share one handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PathsTool {
    Lint,
    LintFix,
    FormatCheck,
    FormatWrite,
}

impl PathsTool {
    pub(super) fn from_name(name: &str) -> Option<Self> {
        match name {
            LINT => Some(Self::Lint),
            LINT_FIX => Some(Self::LintFix),
            FORMAT_CHECK => Some(Self::FormatCheck),
            FORMAT_WRITE => Some(Self::FormatWrite),
            _ => None,
        }
    }

    /// Whether the tool writes (`--fix` / write mode).
    pub(super) fn applies(self) -> bool {
        matches!(self, Self::LintFix | Self::FormatWrite)
    }

    fn is_lint(self) -> bool {
        matches!(self, Self::Lint | Self::LintFix)
    }

    fn run(self, backend: &Backend, call: &PathsCall) -> anyhow::Result<RunReply> {
        if self.is_lint() {
            backend.lint(call)
        } else {
            backend.format(call)
        }
    }

    fn result(self, reply: RunReply, repr: TextRepr) -> CallToolResult {
        if self.is_lint() {
            dto::lint_result(reply, repr)
        } else {
            dto::format_result(reply, repr)
        }
    }
}

/// Annotations for a tool that reads or checks only but may download a component, so it reaches
/// beyond the machine: closed-world would be a false claim.
fn read_only_open_world_annotations() -> ToolAnnotations {
    ToolAnnotations::new()
        .read_only(true)
        .destructive(false)
        .idempotent(true)
        .open_world(true)
}

/// Annotations for a mutating tool that may also download a component.
fn mutating_open_world_annotations() -> ToolAnnotations {
    ToolAnnotations::new()
        .read_only(false)
        .destructive(true)
        .idempotent(false)
        .open_world(true)
}

/// Annotations for the component installer: it writes only the component cache, removes nothing
/// and gives the same answer when repeated, but it does reach the network.
fn installing_annotations() -> ToolAnnotations {
    ToolAnnotations::new()
        .read_only(false)
        .destructive(false)
        .idempotent(true)
        .open_world(true)
}

/// What every lint and format result reports about the run, appended to each tool description.
const RUN_RESULT_NOTE: &str = "A file poly could not process is reported with an `error` (never as clean) and sets `isError`. \
    The result carries `skipped` and a `summary` of checked/skipped/errored counts: check those, not just \
    an empty `results`, since a run that skipped everything reports no diagnostics and is not an error. \
    When the run needed downloadable components the result carries a `components` block (`used`, `installed_now`, \
    `missing`); a missing component that fails the run sets `isError`. `install` (`config`|`auto`|`never`) \
    controls whether a missing engine component is downloaded (recipe tools follow their own \
    `[tools.<name>] install`). The call may download, so it is open-world; with a client \
    that supports Tasks it returns a Task when a download is needed.";

/// The input schema for [`ComponentsInstallParams`].
fn schema_for_install_params() -> Arc<JsonObject> {
    rmcp::handler::server::tool::schema_for_input::<ComponentsInstallParams>()
        .expect("ComponentsInstallParams has a valid input schema")
}

/// A hand-registered lint or format tool: [`PathsParams`] in, the host's document out.
fn paths_tool(name: &'static str, summary: &str, output_schema: Arc<JsonObject>, annotations: ToolAnnotations) -> Tool {
    let schema =
        rmcp::handler::server::tool::schema_for_input::<PathsParams>().expect("PathsParams has a valid input schema");
    Tool::new(name, format!("{summary} {RUN_RESULT_NOTE}"), schema)
        .with_raw_output_schema(output_schema)
        .annotate(annotations)
}

/// The definitions of the hand-registered lint, format and install tools.
pub(super) fn tool_definitions() -> Vec<Tool> {
    vec![
        paths_tool(
            LINT,
            "Lint files and report diagnostics as structured JSON (plus JSON/TOON text). Never writes. Mirrors `poly lint`.",
            dto::lint_output_schema(),
            read_only_open_world_annotations(),
        ),
        paths_tool(
            FORMAT_CHECK,
            "Check formatting without writing; reports which files would change as structured JSON (plus JSON/TOON text). \
             Mirrors `poly fmt` (the dry run).",
            dto::format_output_schema(),
            read_only_open_world_annotations(),
        ),
        paths_tool(
            LINT_FIX,
            "Lint files and apply available autofixes in place, then report remaining diagnostics. Writes files. \
             Mirrors `poly lint --fix`.",
            dto::lint_output_schema(),
            mutating_open_world_annotations(),
        ),
        paths_tool(
            FORMAT_WRITE,
            "Format files in place and report which files changed. Writes files. Mirrors `poly fmt --fix`.",
            dto::format_output_schema(),
            mutating_open_world_annotations(),
        ),
        Tool::new(
            COMPONENTS_INSTALL,
            "Download and verify engine components: `names` (components or presets), `languages`, and/or `paths` (every \
             component those paths need). Writes only the component cache. With a client that supports Tasks, returns a Task \
             when something has to be downloaded (poll `tasks/get`; the status message names the components, there is no \
             byte-level progress). `dry_run` reports what would be downloaded. Fails closed offline. Mirrors `poly components install`.",
            schema_for_install_params(),
        )
        .with_output_schema::<ComponentActions>()
        .annotate(installing_annotations()),
    ]
}

impl PolyMcpServer {
    /// Handle a lint or format call.
    ///
    /// Returns a Task, instead of blocking the call, only when the client declared the tasks
    /// capability and a pre-check finds components this run would download. The pre-check never
    /// downloads and its failure is not the call's: the run reports its own errors.
    pub(super) async fn run_paths_tool(
        &self,
        tool: PathsTool,
        request: CallToolRequestParams,
        context: &RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let args: PathsParams = parse_arguments(request.arguments)?;
        let config = effective_config(args.config, &self.config_override);
        let (repr, install) = (args.format, args.install);
        let call = paths_call(args.paths, args.exclude, config, tool.applies(), install);
        let backend = self.backend.clone();

        let supports_tasks = context.client_capabilities().is_some_and(|caps| caps.supports_tasks());
        let mut pending = Vec::new();
        if supports_tasks && install != InstallMode::Never {
            let probe = ComponentsStatusRequest {
                paths: if call.paths.is_empty() {
                    vec![".".to_owned()]
                } else {
                    call.paths.clone()
                },
                config: call.config.clone(),
                install,
            };
            let backend = backend.clone();
            pending = run_blocking(move || backend.components_status(&probe))
                .await
                .map(|status| status.would_download)
                // A failed pre-check only costs the Task: the run itself reports the real error.
                .unwrap_or_else(|error| {
                    eprintln!("poly mcp: component pre-check failed, running inline: {error:?}");
                    Vec::new()
                });
        }
        if pending.is_empty() {
            let reply = run_blocking(move || tool.run(&backend, &call)).await?;
            return Ok(CallToolResponse::Complete(tool.result(reply, repr)));
        }

        let status = format!("downloading components: {}", pending.join(", "));
        let task = self.task_manager.spawn(
            TaskOptions::new().with_ttl_ms(None).with_status_message(status),
            move |_ctx| {
                Box::pin(async move {
                    let reply = tokio::task::spawn_blocking(move || tool.run(&backend, &call))
                        .await
                        .map_err(|error| {
                            TaskExit::Error(ErrorData::internal_error(format!("task panicked: {error}"), None))
                        })?
                        .map_err(|error| TaskExit::Error(ErrorData::internal_error(format!("{error:#}"), None)))?;
                    Ok(tool.result(reply, repr))
                })
            },
        );
        Ok(CallToolResponse::Task(rmcp::model::CreateTaskResult::new(task)))
    }

    /// Handle `components_install`: a Task when the client supports them and a dry run finds
    /// something to download, otherwise a completed result.
    pub(super) async fn run_components_install(
        &self,
        request: CallToolRequestParams,
        context: &RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let args: ComponentsInstallParams = parse_arguments(request.arguments)?;
        let repr = args.format;
        let mut install = ComponentsInstallRequest {
            names: args.names,
            languages: args.languages,
            paths: args.paths,
            config: effective_config(args.config, &self.config_override),
            dry_run: args.dry_run,
        };
        let backend = self.backend.clone();

        let supports_tasks = context.client_capabilities().is_some_and(|caps| caps.supports_tasks());
        if install.dry_run || !supports_tasks {
            let reply = run_blocking(move || backend.components_install(&install)).await?;
            return Ok(CallToolResponse::Complete(actions_result(&reply, repr)?));
        }

        // The plan decides whether this is a Task. With nothing to download it is the answer:
        // an install would report the same rows.
        install.dry_run = true;
        let plan_request = install.clone();
        let plan_backend = backend.clone();
        let plan = run_blocking(move || plan_backend.components_install(&plan_request)).await?;
        let downloads: Vec<&str> = plan
            .results
            .iter()
            .filter(|row| row.action == "would_install")
            .map(|row| row.name.as_str())
            .collect();
        if downloads.is_empty() {
            return Ok(CallToolResponse::Complete(actions_result(&plan, repr)?));
        }

        let status = format!("downloading components: {}", downloads.join(", "));
        install.dry_run = false;
        let task = self.task_manager.spawn(
            TaskOptions::new().with_ttl_ms(None).with_status_message(status),
            move |_ctx| {
                Box::pin(async move {
                    let reply = tokio::task::spawn_blocking(move || backend.components_install(&install))
                        .await
                        .map_err(|error| {
                            TaskExit::Error(ErrorData::internal_error(
                                format!("install task panicked: {error}"),
                                None,
                            ))
                        })?
                        .map_err(|error| TaskExit::Error(ErrorData::internal_error(format!("{error:#}"), None)))?;
                    actions_result(&reply, repr).map_err(TaskExit::Error)
                })
            },
        );
        Ok(CallToolResponse::Task(rmcp::model::CreateTaskResult::new(task)))
    }
}
