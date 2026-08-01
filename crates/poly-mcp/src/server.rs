//! The MCP server: a [`PolyMcpServer`] exposing the CLI's capabilities as MCP
//! tools over stdio.
//!
//! ## Output shape
//!
//! Every tool returns rmcp **structured content**: the typed payload lands in
//! `CallToolResult.structured_content` (its JSON schema is attached to the tool
//! via `#[tool(output_schema = …)]`), and a single text block carries the same
//! data as JSON or compact **TOON** (`format` param). See [`crate::dto`].
//!
//! ## Annotations
//!
//! Annotations are static per tool, so read-only and mutating variants are split
//! into **separate** tools rather than gated behind a boolean: `lint` /
//! `format_check` / `cache_stats` / `rules` / `config_show` are read-only and
//! idempotent, while `lint_fix` / `format_write` / `cache_clean` are destructive.
//!
//! ## Async Tasks
//!
//! The whole-project tools (`workspace_lint` / `workspace_lint_fix`) drive the
//! multi-minute `cargo clippy` / `cargo-deny` phase and are exposed as SEP-2663
//! **async Tasks** (via [`rmcp::task_manager`]): the call returns a task handle
//! and the client polls `tasks/get`. A client that does not declare the tasks
//! capability transparently gets a synchronous (blocking) result instead. These
//! two tools are registered manually (a `#[tool]` fn cannot return a task), so
//! the [`ServerHandler`] impl is hand-written rather than `#[tool_handler]`.

use std::path::PathBuf;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, CancelTaskParams, GetTaskParams, GetTaskResult,
    Implementation, ListToolsResult, PaginatedRequestParams, ResultType, ServerCapabilities, ServerInfo, Tool,
    ToolAnnotations, UpdateTaskParams,
};
use rmcp::service::RequestContext;
use rmcp::task_manager::{TaskExit, TaskManager, TaskOptions};
use rmcp::{ErrorData, RoleServer, ServerHandler, tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::dto::{FormatReport, LintReport, TextRepr, WorkspaceReport, dto_result};
use crate::ops;

/// Tool name for the read-only whole-project lint Task.
const WORKSPACE_LINT: &str = "workspace_lint";
/// Tool name for the mutating whole-project lint Task.
const WORKSPACE_LINT_FIX: &str = "workspace_lint_fix";

/// Arguments accepted by the path-oriented lint/format tools.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct PathsParams {
    /// Files or directories to process. Empty means the current directory.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Optional path to a config file (`poly.toml`). When omitted, the server's
    /// `--config` override is used, otherwise config is discovered from the
    /// working directory like the CLI.
    #[serde(default)]
    pub config: Option<String>,
    /// Gitignore-style globs to exclude from discovery, merged with the config's
    /// `[discovery] exclude`. Mirrors the CLI `--exclude` flag.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Text representation for the result's text content block (`structured_content`
    /// stays JSON). `json` (default) or `toon`.
    #[serde(default)]
    pub format: TextRepr,
}

/// Arguments for the parameterless cache tools: only the text representation.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct FormatParams {
    /// Text representation for the result's text content block. `json` (default)
    /// or `toon`.
    #[serde(default)]
    pub format: TextRepr,
}

/// Arguments for `config_show`.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct ConfigParams {
    /// Optional path to a config file (`poly.toml`).
    #[serde(default)]
    pub config: Option<String>,
    /// Text representation for the result's text content block.
    #[serde(default)]
    pub format: TextRepr,
}

/// Arguments for `rules`.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct RulesParams {
    /// Rule directories to search. Empty means `[rules] dirs` from the config.
    #[serde(default)]
    pub dirs: Vec<String>,
    /// Optional path to a config file (used to resolve `[rules] dirs`).
    #[serde(default)]
    pub config: Option<String>,
    /// When true, also run each rule's `*-test.yml` snippets and report outcomes.
    #[serde(default)]
    pub test: bool,
    /// Text representation for the result's text content block.
    #[serde(default)]
    pub format: TextRepr,
}

/// Arguments for the whole-project (workspace) Task tools.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct WorkspaceParams {
    /// Optional path to a config file (`poly.toml`).
    #[serde(default)]
    pub config: Option<String>,
    /// The `-j` concurrency override for the whole-project tools.
    #[serde(default)]
    pub jobs: Option<usize>,
    /// Bypass the result cache (`--no-cache`).
    #[serde(default)]
    pub no_cache: bool,
    /// Text representation for the completed result's text content block.
    #[serde(default)]
    pub format: TextRepr,
}

/// MCP server mirroring the `poly` CLI's lint/format/cache/rules/config and
/// whole-project capabilities.
#[derive(Clone)]
pub struct PolyMcpServer {
    tool_router: ToolRouter<PolyMcpServer>,
    /// Store and executor for the whole-project async Tasks.
    task_manager: TaskManager,
    /// Config path passed on the command line (`poly mcp --config`); used as the
    /// fallback when a request does not name its own config.
    config_override: Option<PathBuf>,
}

/// Resolve the effective config path for a request: an explicit per-request
/// path wins, otherwise the server-wide override (if any).
fn effective_config(request: Option<String>, server: &Option<PathBuf>) -> Option<String> {
    request.or_else(|| server.as_ref().map(|p| p.display().to_string()))
}

/// Run a synchronous engine operation on a blocking task and map failures onto
/// an MCP internal error.
async fn run_blocking<F, T>(operation: F) -> Result<T, ErrorData>
where
    F: FnOnce() -> anyhow::Result<T> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(operation).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(ErrorData::internal_error(format!("{error:#}"), None)),
        Err(join_error) => Err(ErrorData::internal_error(
            format!("engine task panicked: {join_error}"),
            None,
        )),
    }
}

/// Annotations for a read-only, idempotent, closed-world tool.
fn read_only_annotations() -> ToolAnnotations {
    ToolAnnotations::new()
        .read_only(true)
        .destructive(false)
        .idempotent(true)
        .open_world(false)
}

/// Annotations for a mutating (destructive), closed-world tool.
fn mutating_annotations() -> ToolAnnotations {
    ToolAnnotations::new()
        .read_only(false)
        .destructive(true)
        .idempotent(false)
        .open_world(false)
}

#[tool_router]
impl PolyMcpServer {
    /// Build a server, optionally pinning a config file used for every request
    /// that does not name its own.
    pub fn new(config_override: Option<PathBuf>) -> Self {
        Self {
            tool_router: Self::tool_router(),
            task_manager: TaskManager::new(),
            config_override,
        }
    }

    #[tool(
        description = "Lint files and report diagnostics as structured JSON (plus JSON/TOON text). Never writes. Mirrors `poly lint`.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false),
        output_schema = rmcp::handler::server::tool::schema_for_output::<LintReport>()
    )]
    async fn lint(&self, params: Parameters<PathsParams>) -> Result<CallToolResult, ErrorData> {
        let Parameters(args) = params;
        let config = effective_config(args.config, &self.config_override);
        let repr = args.format;
        let results =
            run_blocking(move || ops::lint_results(&args.paths, &args.exclude, config.as_deref(), false)).await?;
        LintReport { results }.into_result(repr)
    }

    #[tool(
        description = "Check formatting without writing; reports which files would change as structured JSON (plus JSON/TOON text). Mirrors `poly fmt --check`.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false),
        output_schema = rmcp::handler::server::tool::schema_for_output::<FormatReport>()
    )]
    async fn format_check(&self, params: Parameters<PathsParams>) -> Result<CallToolResult, ErrorData> {
        let Parameters(args) = params;
        let config = effective_config(args.config, &self.config_override);
        let repr = args.format;
        let results =
            run_blocking(move || ops::format_results(&args.paths, &args.exclude, config.as_deref(), false)).await?;
        FormatReport { results }.into_result(repr)
    }

    #[tool(
        description = "Lint files and apply available autofixes in place, then report remaining diagnostics. Writes files. Mirrors `poly lint --fix`.",
        annotations(read_only_hint = false, destructive_hint = true, open_world_hint = false),
        output_schema = rmcp::handler::server::tool::schema_for_output::<LintReport>()
    )]
    async fn lint_fix(&self, params: Parameters<PathsParams>) -> Result<CallToolResult, ErrorData> {
        let Parameters(args) = params;
        let config = effective_config(args.config, &self.config_override);
        let repr = args.format;
        let results =
            run_blocking(move || ops::lint_results(&args.paths, &args.exclude, config.as_deref(), true)).await?;
        LintReport { results }.into_result(repr)
    }

    #[tool(
        description = "Format files in place and report which files changed. Writes files. Mirrors `poly fmt --fix`.",
        annotations(read_only_hint = false, destructive_hint = true, open_world_hint = false),
        output_schema = rmcp::handler::server::tool::schema_for_output::<FormatReport>()
    )]
    async fn format_write(&self, params: Parameters<PathsParams>) -> Result<CallToolResult, ErrorData> {
        let Parameters(args) = params;
        let config = effective_config(args.config, &self.config_override);
        let repr = args.format;
        let results =
            run_blocking(move || ops::format_results(&args.paths, &args.exclude, config.as_deref(), true)).await?;
        FormatReport { results }.into_result(repr)
    }

    #[tool(
        description = "Report result-cache footprint (entry counts, sizes, format version). Mirrors `poly cache stats`.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false),
        output_schema = rmcp::handler::server::tool::schema_for_output::<crate::dto::CacheStatsReport>()
    )]
    async fn cache_stats(&self, params: Parameters<FormatParams>) -> Result<CallToolResult, ErrorData> {
        let Parameters(args) = params;
        let report = run_blocking(ops::cache_stats).await?;
        dto_result(&report, args.format)
    }

    #[tool(
        description = "Remove every cached entry and report freed bytes. Mirrors `poly cache clean`.",
        annotations(read_only_hint = false, destructive_hint = true, open_world_hint = false),
        output_schema = rmcp::handler::server::tool::schema_for_output::<crate::dto::CacheCleanReport>()
    )]
    async fn cache_clean(&self, params: Parameters<FormatParams>) -> Result<CallToolResult, ErrorData> {
        let Parameters(args) = params;
        let report = run_blocking(ops::cache_clean).await?;
        dto_result(&report, args.format)
    }

    #[tool(
        description = "List (and optionally test) the custom ast-grep rule packs. Read-only. Mirrors `poly rules list` / `poly rules test`.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false),
        output_schema = rmcp::handler::server::tool::schema_for_output::<crate::dto::RulesReport>()
    )]
    async fn rules(&self, params: Parameters<RulesParams>) -> Result<CallToolResult, ErrorData> {
        let Parameters(args) = params;
        let config = effective_config(args.config, &self.config_override);
        let repr = args.format;
        let report = run_blocking(move || ops::rules_report(&args.dirs, config.as_deref(), args.test)).await?;
        dto_result(&report, repr)
    }

    #[tool(
        description = "Show the effective, merged config (defaults, configured lint/fmt/tool keys, rule dirs). Read-only. Mirrors `poly config show` (network-free: remote `extends` bases are not fetched).",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false),
        output_schema = rmcp::handler::server::tool::schema_for_output::<crate::dto::ConfigShowReport>()
    )]
    async fn config_show(&self, params: Parameters<ConfigParams>) -> Result<CallToolResult, ErrorData> {
        let Parameters(args) = params;
        let config = effective_config(args.config, &self.config_override);
        let repr = args.format;
        let report = run_blocking(move || ops::config_show(config.as_deref())).await?;
        dto_result(&report, repr)
    }
}

impl PolyMcpServer {
    /// Definitions for the two whole-project Task tools, which are not `#[tool]`
    /// methods (a macro tool cannot return a task handle).
    fn task_tool_definitions(&self) -> Vec<Tool> {
        vec![
            Tool::new(
                WORKSPACE_LINT,
                "Run the whole-project lint phase (cargo clippy / cargo-sort / cargo-machete / cargo-deny and inline whole-project jobs) in check mode. \
                 Long-running: exposed as an async Task — poll `tasks/get` for the structured result. Read-only.",
                schema_for_workspace_params(),
            )
            .with_output_schema::<WorkspaceReport>()
            .annotate(read_only_annotations()),
            Tool::new(
                WORKSPACE_LINT_FIX,
                "Run the whole-project lint phase in fix mode (cargo sort in place, cargo-machete --fix, cargo clippy --fix). Writes files. \
                 Long-running: exposed as an async Task — poll `tasks/get` for the structured result.",
                schema_for_workspace_params(),
            )
            .with_output_schema::<WorkspaceReport>()
            .annotate(mutating_annotations()),
        ]
    }

    /// Handle a whole-project tool call. Spawns an async Task when the client
    /// declared the tasks capability; otherwise runs synchronously (blocking) and
    /// returns a completed result so any client can use the tool.
    async fn run_workspace_tool(
        &self,
        request: CallToolRequestParams,
        context: &RequestContext<RoleServer>,
        fix: bool,
    ) -> Result<CallToolResponse, ErrorData> {
        let args: WorkspaceParams = parse_arguments(request.arguments)?;
        let config = effective_config(args.config, &self.config_override);
        let repr = args.format;
        let (jobs, no_cache) = (args.jobs, args.no_cache);

        let supports_tasks = context.client_capabilities().is_some_and(|caps| caps.supports_tasks());
        if !supports_tasks {
            // Synchronous fallback for clients without the tasks extension.
            let report = run_blocking(move || ops::workspace_lint(config.as_deref(), fix, jobs, no_cache)).await?;
            return Ok(CallToolResponse::Complete(dto_result(&report, repr)?));
        }

        let status = if fix {
            "running whole-project lint (fix mode)"
        } else {
            "running whole-project lint"
        };
        // Unlimited TTL: the cargo phase can legitimately run for minutes, past
        // the default 5-minute TTL that would otherwise mark it failed.
        let task = self.task_manager.spawn(
            TaskOptions::new().with_ttl_ms(None).with_status_message(status),
            move |_ctx| {
                Box::pin(async move {
                    let report = tokio::task::spawn_blocking(move || {
                        ops::workspace_lint(config.as_deref(), fix, jobs, no_cache)
                    })
                    .await
                    .map_err(|error| {
                        TaskExit::Error(ErrorData::internal_error(
                            format!("workspace task panicked: {error}"),
                            None,
                        ))
                    })?
                    .map_err(|error| TaskExit::Error(ErrorData::internal_error(format!("{error:#}"), None)))?;
                    dto_result(&report, repr).map_err(TaskExit::Error)
                })
            },
        );
        Ok(CallToolResponse::Task(rmcp::model::CreateTaskResult::new(task)))
    }
}

/// The input schema for [`WorkspaceParams`], used by the manually-registered
/// task tool definitions.
fn schema_for_workspace_params() -> std::sync::Arc<rmcp::model::JsonObject> {
    rmcp::handler::server::tool::schema_for_input::<WorkspaceParams>()
        .expect("WorkspaceParams has a valid input schema")
}

/// Deserialize a tool call's `arguments` map into a typed params struct,
/// defaulting to an empty object when the caller passed none.
fn parse_arguments<T: for<'de> Deserialize<'de>>(arguments: Option<rmcp::model::JsonObject>) -> Result<T, ErrorData> {
    let value = serde_json::Value::Object(arguments.unwrap_or_default());
    serde_json::from_value(value)
        .map_err(|error| ErrorData::invalid_params(format!("invalid arguments: {error}"), None))
}

impl ServerHandler for PolyMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().enable_tasks().build())
            .with_server_info(Implementation::new("poly-mcp", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Universal zero-dependency linter & formatter. Tools mirror the `poly` CLI. Read-only: \
                 lint / format_check / cache_stats / rules / config_show. Mutating: lint_fix / format_write / \
                 cache_clean. Whole-project (long-running): workspace_lint / workspace_lint_fix, run as async \
                 Tasks (poll tasks/get). Every tool returns structured JSON plus a JSON/TOON text block (`format`).",
            )
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        match request.name.as_ref() {
            WORKSPACE_LINT => self.run_workspace_tool(request, &context, false).await,
            WORKSPACE_LINT_FIX => self.run_workspace_tool(request, &context, true).await,
            _ => {
                let tcc = ToolCallContext::new(self, request, context);
                self.tool_router.call(tcc).await
            }
        }
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let mut tools = self.tool_router.list_all();
        tools.extend(self.task_tool_definitions());
        tools.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(ListToolsResult {
            result_type: Some(ResultType::COMPLETE),
            tools,
            meta: None,
            next_cursor: None,
            ttl_ms: None,
            cache_scope: None,
        })
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tool_router
            .get(name)
            .cloned()
            .or_else(|| self.task_tool_definitions().into_iter().find(|tool| tool.name == name))
    }

    async fn get_task(
        &self,
        request: GetTaskParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetTaskResult, ErrorData> {
        self.task_manager.get_task(&request.task_id).map(GetTaskResult::new)
    }

    async fn update_task(
        &self,
        request: UpdateTaskParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        self.task_manager.update_task(&request.task_id, request.input_responses)
    }

    async fn cancel_task(
        &self,
        request: CancelTaskParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        self.task_manager.cancel_task(&request.task_id)
    }
}

impl PolyMcpServer {
    /// Names of every registered tool (router tools plus the task tools).
    pub fn tool_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .tool_router
            .list_all()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect();
        names.extend(
            self.task_tool_definitions()
                .into_iter()
                .map(|tool| tool.name.to_string()),
        );
        names
    }

    /// `(read_only_hint, destructive_hint)` for a named tool, if registered.
    pub fn tool_hints(&self, name: &str) -> Option<(Option<bool>, Option<bool>)> {
        self.get_tool(name).map(|tool| {
            let annotations = tool.annotations.as_ref();
            (
                annotations.and_then(|a| a.read_only_hint),
                annotations.and_then(|a| a.destructive_hint),
            )
        })
    }
}
