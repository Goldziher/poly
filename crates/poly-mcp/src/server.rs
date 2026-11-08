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
//! The lint/format tools report three distinct per-file outcomes — checked,
//! skipped (`skipped`), and **failed** (`error`) — and set `isError` when any
//! file failed. An MCP caller has no exit code, so a file poly could not process
//! has to be visible in the payload itself or the caller reads the run as clean.
//!
//! ## Annotations
//!
//! Annotations are static per tool, so read-only and mutating variants are split
//! into **separate** tools rather than gated behind a boolean: `lint` /
//! `format_check` / `cache_stats` / `rules` / `config_show` are read-only and
//! idempotent, while `lint_fix` / `format_write` / `cache_clean` are destructive.
//!
//! ## Identity
//!
//! Every result carries the serving binary's identity (see [`crate::identity`])
//! under a `poly` key in `structured_content` and in `_meta`, because an MCP
//! caller has no `poly --version` to fall back on. The server additionally
//! fingerprints its own executable at startup and re-checks it per request: if
//! the binary is replaced or deleted underneath a long-lived server, every tool
//! but `version` fails rather than answering with superseded behaviour.
//!
//! ## Component downloads
//!
//! `lint`, `format_check`, `lint_fix`, `format_write` and `components_install` are registered by
//! hand, like the whole-project tools, because a `#[tool]` fn cannot return a Task. They return a
//! Task only when the client declared the tasks capability and a probe (`components.status`,
//! which never downloads) finds components the run would download; otherwise the call blocks and
//! returns the result. The Task carries a status message naming the components. There is no
//! byte-level progress: host services are component-to-host only and one blocking call, so the
//! host has no way to report mid-call, and `notifications/progress` is not sent.
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
use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, CancelTaskParams, GetTaskParams, GetTaskResult,
    Implementation, ListToolsResult, PaginatedRequestParams, ResultType, ServerCapabilities, ServerConfig, Tool,
    ToolAnnotations, UpdateTaskParams,
};
use rmcp::service::RequestContext;
use rmcp::task_manager::{TaskExit, TaskManager, TaskOptions};
use rmcp::{ErrorData, RoleServer, ServerHandler, tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

use poly_frontend_api::ServiceCaller;
use poly_frontend_api::wire::{
    ComponentsStatusRequest, ComponentsUninstallRequest, ConfigRequest, InstallMode, PathsRequest as PathsCall,
    RulesRequest, WorkspaceRequest,
};

mod components;

use crate::backend::Backend;
use crate::dto;
use crate::dto::{
    ComponentActions, ComponentsStatus, TextRepr, VersionReport, WorkspaceReport, actions_result, dto_result,
    schema_for_output_with_identity,
};
use crate::identity::{self, ExecutableWatch};
use components::{COMPONENTS_INSTALL, PathsTool};
pub use components::{ComponentsInstallParams, ComponentsParams, ComponentsUninstallParams};

/// Tool name for the check-mode whole-project lint Task.
///
/// Check mode means poly requests no fixes, not that the worktree is untouched:
/// the phase executes the configured tools, whose own side effects poly does not
/// control (see [`poly_workspace::run_workspace_lint`]). It is therefore
/// annotated [`side_effecting_annotations`] rather than read-only — the prose
/// caveat is not what an auto-approving client reads.
const WORKSPACE_LINT: &str = "workspace_lint";
/// Tool name for the mutating whole-project lint Task.
const WORKSPACE_LINT_FIX: &str = "workspace_lint_fix";
/// Tool name for the identity/health tool, which answers even when the
/// executable has moved (it is the tool that explains why everything else
/// stopped answering).
const VERSION: &str = "version";

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
    ///
    /// A glob without a leading `/` matches a directory of that name at any
    /// depth (`"e2e/**"` also prunes `src/test/java/io/xberg/e2e/`); a leading
    /// `/` anchors it to the config directory (`"/e2e/**"`).
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Whether a component this run needs may be downloaded: `config` (default) follows
    /// `[components] install` in the config, `auto` downloads whatever is missing, `never`
    /// downloads nothing and skips the files a missing component would have handled. A server
    /// started with `--offline` never downloads, whatever this says. It governs engine components
    /// only: an opt-in recipe tool with `[tools.<name>] install = "auto"` follows its own setting.
    #[serde(default)]
    pub install: InstallMode,
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
    /// User rule directories to search. Empty means `[rules] dirs` from the
    /// config. The built-in pack is listed either way, unless the config sets
    /// `[rules] builtin = false`.
    #[serde(default)]
    pub dirs: Vec<String>,
    /// Optional path to a config file (used to resolve `[rules] dirs`, `[rules]
    /// builtin`, and the `[lint.astgrep]` rule selection).
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
    /// The host's services, the only way this server reaches an engine.
    backend: Backend,
    /// Config path passed on the command line (`poly mcp --config`); used as the
    /// fallback when a request does not name its own config.
    config_override: Option<PathBuf>,
    /// Fingerprint of the executable this server started from, re-checked on
    /// every request so an upgrade underneath a running server fails loudly
    /// instead of answering with superseded behaviour.
    executable: ExecutableWatch,
}

/// The host request for a lint or format call; `apply` is `--fix` / write.
fn paths_call(
    paths: Vec<String>,
    exclude: Vec<String>,
    config: Option<String>,
    apply: bool,
    install: InstallMode,
) -> PathsCall {
    PathsCall {
        paths,
        exclude,
        config,
        apply,
        install,
    }
}

/// Teach the process the host's engine versions and output schemas. A failure leaves the
/// defaults (an empty engine digest, bare output schemas) rather than refusing to serve: the
/// tools themselves report the host's error when it matters.
fn install_host_metadata(backend: &Backend) {
    match backend.engines() {
        Ok(engines) => identity::install_host(&engines),
        Err(error) => eprintln!("poly mcp: cannot read the host's engine versions: {error:#}"),
    }
    match backend.schemas() {
        Ok(schemas) => dto::install_document_schemas(schemas),
        Err(error) => eprintln!("poly mcp: cannot read the host's output schemas: {error:#}"),
    }
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

/// Annotations for a tool that applies no fixes of its own but still executes
/// third-party tools against the live worktree.
///
/// `read_only` is false because the tree can change — a refreshed lock file, a
/// populated build cache. It is not `destructive`: nothing is deleted or
/// overwritten with poly's own output. Clients use `read_only_hint` for
/// auto-approval, so claiming it here would let the one case that actually
/// matters — a tree that changes without a prompt — through unannounced.
fn side_effecting_annotations() -> ToolAnnotations {
    ToolAnnotations::new()
        .read_only(false)
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
    /// Build a server over the host's services, optionally pinning a config file used for every
    /// request that does not name its own.
    pub fn new(caller: Arc<dyn ServiceCaller>, config_override: Option<PathBuf>) -> Self {
        Self::with_executable_watch(caller, config_override, ExecutableWatch::capture())
    }

    /// Build a server watching a specific executable rather than the running
    /// one.
    ///
    /// [`new`](Self::new) is the normal entry point. This takes the watch as an
    /// explicit dependency for an embedder that knows which file backs the
    /// server — a supervisor watching the binary it launched, or a harness
    /// exercising the replaced-underneath-a-running-server path.
    pub fn with_executable_watch(
        caller: Arc<dyn ServiceCaller>,
        config_override: Option<PathBuf>,
        executable: ExecutableWatch,
    ) -> Self {
        let backend = Backend::new(caller);
        install_host_metadata(&backend);
        Self {
            backend,
            tool_router: Self::tool_router(),
            task_manager: TaskManager::new(),
            config_override,
            executable,
        }
    }

    #[tool(
        description = "Report result-cache footprint (entry counts, sizes, format version). Mirrors `poly cache stats`.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false),
        output_schema = schema_for_output_with_identity::<crate::dto::CacheStatsReport>()
    )]
    async fn cache_stats(&self, params: Parameters<FormatParams>) -> Result<CallToolResult, ErrorData> {
        let Parameters(args) = params;
        let backend = self.backend.clone();
        let report = run_blocking(move || backend.cache_stats()).await?;
        dto_result(&report, args.format)
    }

    #[tool(
        description = "Remove every cached entry and report freed bytes. Mirrors `poly cache clean`.",
        annotations(read_only_hint = false, destructive_hint = true, open_world_hint = false),
        output_schema = schema_for_output_with_identity::<crate::dto::CacheCleanReport>()
    )]
    async fn cache_clean(&self, params: Parameters<FormatParams>) -> Result<CallToolResult, ErrorData> {
        let Parameters(args) = params;
        let backend = self.backend.clone();
        let report = run_blocking(move || backend.cache_clean()).await?;
        dto_result(&report, args.format)
    }

    #[tool(
        description = "List (and optionally test) the ast-grep rules a run would apply: poly's built-in rule pack plus \
                      user rules from `[rules] dirs`. Each rule carries its language, source (`builtin`/`user`), declared \
                      default severity, the severity it reports at under the resolved config, and whether it is enabled. \
                      Read-only. Mirrors `poly rules list` / `poly rules test`.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false),
        output_schema = schema_for_output_with_identity::<crate::dto::RulesReport>()
    )]
    async fn rules(&self, params: Parameters<RulesParams>) -> Result<CallToolResult, ErrorData> {
        let Parameters(args) = params;
        let config = effective_config(args.config, &self.config_override);
        let repr = args.format;
        let backend = self.backend.clone();
        let request = RulesRequest {
            dirs: args.dirs,
            config,
            test: args.test,
        };
        let report = run_blocking(move || backend.rules(&request)).await?;
        dto_result(&report, repr)
    }

    #[tool(
        description = "Show the effective, merged config (defaults, configured lint/fmt/tool keys, rule dirs). Read-only. Mirrors `poly config show` (network-free: remote `extends` bases are not fetched).",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false),
        output_schema = schema_for_output_with_identity::<crate::dto::ConfigShowReport>()
    )]
    async fn config_show(&self, params: Parameters<ConfigParams>) -> Result<CallToolResult, ErrorData> {
        let Parameters(args) = params;
        let config = effective_config(args.config, &self.config_override);
        let repr = args.format;
        let backend = self.backend.clone();
        let report = run_blocking(move || backend.config_show(&ConfigRequest { config })).await?;
        dto_result(&report, repr)
    }

    #[tool(
        description = "Report the downloadable engine components and where each stands (bundled, installed, not installed, \
                      unavailable). With `paths`, only the components those paths need, which are missing, and which a run \
                      would download under `install`. Never downloads. Mirrors `poly components list` / `status`.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false),
        output_schema = schema_for_output_with_identity::<ComponentsStatus>()
    )]
    async fn components(&self, params: Parameters<ComponentsParams>) -> Result<CallToolResult, ErrorData> {
        let Parameters(args) = params;
        let request = ComponentsStatusRequest {
            paths: args.paths,
            config: effective_config(args.config, &self.config_override),
            install: args.install,
        };
        let backend = self.backend.clone();
        let report = run_blocking(move || backend.components_status(&request)).await?;
        dto_result(&report, args.format)
    }

    #[tool(
        description = "Remove downloaded components (names or presets), and with `stale` the cached archives nothing \
                      references or used recently. Refuses a component loaded in this process (`refused`, and `isError`). \
                      Never downloads. Mirrors `poly components uninstall` / `prune`.",
        annotations(read_only_hint = false, destructive_hint = true, open_world_hint = false),
        output_schema = schema_for_output_with_identity::<ComponentActions>()
    )]
    async fn components_uninstall(
        &self,
        params: Parameters<ComponentsUninstallParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let Parameters(args) = params;
        let request = ComponentsUninstallRequest {
            names: args.names,
            stale: args.stale,
        };
        let backend = self.backend.clone();
        let report = run_blocking(move || backend.components_uninstall(&request)).await?;
        actions_result(&report, args.format)
    }

    #[tool(
        name = "version",
        description = "Report which poly binary is serving this session (version, build id, channel, executable, pid) and whether that executable is still the one on disk. Answers even when the binary has moved — it is the tool that explains why the others stopped. Read-only.",
        annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false),
        output_schema = schema_for_output_with_identity::<VersionReport>()
    )]
    async fn version(&self, params: Parameters<FormatParams>) -> Result<CallToolResult, ErrorData> {
        let Parameters(args) = params;
        let staleness = self.executable.check();
        let backend = self.backend.clone();
        // The host lookups may fail exactly when this tool is needed to explain why; report the
        // failure in the status instead of failing the call.
        let (engines, components, host_error) = run_blocking(move || {
            Ok(match (backend.engines(), backend.components()) {
                (Ok(engines), Ok(components)) => (engines.engines, components, None),
                (Err(error), _) | (_, Err(error)) => (Default::default(), Vec::new(), Some(format!("{error:#}"))),
            })
        })
        .await?;
        let executable_current = staleness.is_none();
        let mut executable_status = match staleness {
            Some(staleness) => self.executable.stale_message(staleness),
            None => "the executable serving this session is still the file on disk".to_string(),
        };
        if let Some(error) = host_error {
            executable_status.push_str(&format!("; the host could not report engines and components: {error}"));
        }
        let report = VersionReport {
            executable_current,
            executable_status,
            uptime_seconds: self.executable.uptime_seconds(),
            engines,
            components,
        };
        dto_result(&report, args.format)
    }
}

impl PolyMcpServer {
    /// Definitions for the two whole-project Task tools, which are not `#[tool]`
    /// methods (a macro tool cannot return a task handle).
    fn task_tool_definitions(&self) -> Vec<Tool> {
        let mut tools = vec![
            Tool::new(
                WORKSPACE_LINT,
                "Run the whole-project lint phase (cargo clippy / cargo-sort / cargo-machete / cargo-deny and inline whole-project jobs) in check mode. \
                 Applies no fixes, but it executes those tools against the live worktree: their own side effects (a refreshed lock file, a populated \
                 build or type-checker cache) are not poly's to control, so the tree can change. Covers the whole repository — it takes no path list. \
                 Long-running: exposed as an async Task — poll `tasks/get` for the structured result.",
                schema_for_workspace_params(),
            )
            .with_output_schema::<WorkspaceReport>()
            .annotate(side_effecting_annotations()),
            Tool::new(
                WORKSPACE_LINT_FIX,
                "Run the whole-project lint phase in fix mode (cargo sort in place, cargo-machete --fix, cargo clippy --fix). Writes files. \
                 Long-running: exposed as an async Task — poll `tasks/get` for the structured result.",
                schema_for_workspace_params(),
            )
            .with_output_schema::<WorkspaceReport>()
            .annotate(mutating_annotations()),
        ];
        tools.extend(components::tool_definitions());
        tools
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
        let request = WorkspaceRequest {
            config,
            fix,
            jobs: args.jobs,
            no_cache: args.no_cache,
        };
        let backend = self.backend.clone();

        let supports_tasks = context.client_capabilities().is_some_and(|caps| caps.supports_tasks());
        if !supports_tasks {
            // Synchronous fallback for clients without the tasks extension.
            let report = run_blocking(move || backend.workspace_lint(&request)).await?;
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
                    let report = tokio::task::spawn_blocking(move || backend.workspace_lint(&request))
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

impl PolyMcpServer {
    /// Refuse the request when the executable this server runs from is no longer
    /// the file on disk.
    ///
    /// An MCP server is long-lived **by design** and has no expiry: on macOS the
    /// kernel keeps a deleted inode alive for as long as a process holds it, so
    /// a server started before an upgrade keeps serving the pre-upgrade build
    /// indefinitely — answering, plausibly, with behaviour that was fixed hours
    /// ago. Erroring is the only outcome a caller cannot mistake for a result.
    fn reject_if_stale(&self) -> Result<(), ErrorData> {
        let Some(staleness) = self.executable.check() else {
            return Ok(());
        };
        let message = self.executable.stale_message(staleness);
        // Also surface it on stderr, once, for whoever is reading the server's
        // log rather than the tool result.
        static ANNOUNCED: std::sync::Once = std::sync::Once::new();
        ANNOUNCED.call_once(|| eprintln!("poly mcp: {message}"));
        Err(ErrorData::internal_error(message, None))
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
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().enable_tasks().build())
            .with_server_info(Implementation::new("poly-mcp", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "polylint: one linter & formatter for every language in the repository. Tools mirror the \
                 `poly` CLI. Read-only: \
                 lint / format_check / cache_stats / rules / config_show / components / version. Mutating: lint_fix / \
                 format_write / cache_clean / components_uninstall. Component management: components (status, never \
                 downloads), components_install (downloads; a Task when the client supports them and something must be \
                 fetched), components_uninstall (refuses a component loaded in this process). Whole-project \
                 (long-running): workspace_lint / workspace_lint_fix, run as async Tasks (poll tasks/get). \
                 Engines may ship as downloadable components: lint / format_check / lint_fix / format_write take \
                 `install` (`config`|`auto`|`never`) and are open-world, since a run that needs a missing component \
                 may download it (never offline, never under `install: never`); with a Tasks-capable client such a \
                 call returns a Task whose status message names the components (no byte-level progress). \
                 Results carry a `components` block (`used`, `installed_now`, `missing`) when a run needed any; a \
                 missing component that fails the run sets `isError`, while one skipped under `install: never` \
                 shows in `skipped` and `components.missing`. Every tool returns structured JSON plus a JSON/TOON \
                 text block (`format`), and every result carries a `poly` block (version, build id, channel, \
                 flavor, components_lock, executable, pid) identifying the binary that answered — check it before \
                 acting on a result. \
                 lint/format results tell three per-file outcomes apart — checked, `skipped`, and `error` \
                 (poly failed on the file, so it was NOT checked) — and set `isError` when any file failed. \
                 They also carry top-level `errors` and `skipped` lists and a `summary` of \
                 checked/skipped/errored counts. Gate on `summary.checked`, not on an empty `results`: a run \
                 that skipped every file reports no diagnostics and is not an error, so it is otherwise \
                 indistinguishable from a clean pass. The counts are not a partition of `results` — a file \
                 checked and found clean produces no record at all. \
                 If this server's executable is replaced or deleted while it runs, every tool but `version` \
                 fails until the server is restarted.",
            )
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        // `version` is exempt: it is the tool that *explains* a stale server,
        // and refusing it would leave a caller with an error and no diagnosis.
        if request.name.as_ref() != VERSION {
            self.reject_if_stale()?;
        }
        if let Some(tool) = PathsTool::from_name(request.name.as_ref()) {
            return self.run_paths_tool(tool, request, &context).await;
        }
        match request.name.as_ref() {
            WORKSPACE_LINT => self.run_workspace_tool(request, &context, false).await,
            WORKSPACE_LINT_FIX => self.run_workspace_tool(request, &context, true).await,
            COMPONENTS_INSTALL => self.run_components_install(request, &context).await,
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
