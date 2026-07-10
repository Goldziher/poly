//! The MCP server: a [`PolyMcpServer`] exposing the CLI's capabilities as MCP
//! tools over stdio.
//!
//! Annotations are static per tool, so read-only and mutating variants are
//! split into **separate** tools rather than gated behind a `fix`/`write`
//! boolean: `lint` / `format_check` / `cache_stats` are `read_only`, while
//! `lint_fix` / `format_write` / `cache_clean` are `destructive`. Every tool
//! returns exactly the JSON the CLI produces under `--format json`.

use std::path::PathBuf;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{Implementation, ServerCapabilities, ServerInfo};
use rmcp::{ErrorData, ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::ops;

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
}

/// MCP server mirroring the `poly` CLI's lint/format/cache capabilities.
#[derive(Clone)]
pub struct PolyMcpServer {
    tool_router: ToolRouter<PolyMcpServer>,
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
async fn run_blocking<F>(operation: F) -> Result<String, ErrorData>
where
    F: FnOnce() -> anyhow::Result<String> + Send + 'static,
{
    match tokio::task::spawn_blocking(operation).await {
        Ok(Ok(json)) => Ok(json),
        Ok(Err(error)) => Err(ErrorData::internal_error(format!("{error:#}"), None)),
        Err(join_error) => Err(ErrorData::internal_error(
            format!("engine task panicked: {join_error}"),
            None,
        )),
    }
}

#[tool_router]
impl PolyMcpServer {
    /// Build a server, optionally pinning a config file used for every request
    /// that does not name its own.
    pub fn new(config_override: Option<PathBuf>) -> Self {
        Self {
            tool_router: Self::tool_router(),
            config_override,
        }
    }

    #[tool(
        description = "Lint files and report diagnostics as JSON. Never writes. Mirrors `poly lint --format json`.",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn lint(&self, params: Parameters<PathsParams>) -> Result<String, ErrorData> {
        let Parameters(args) = params;
        let config = effective_config(args.config, &self.config_override);
        run_blocking(move || ops::lint(&args.paths, &args.exclude, config.as_deref(), false)).await
    }

    #[tool(
        description = "Check formatting without writing. Reports which files would change as JSON. Mirrors `poly fmt --format json`.",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn format_check(&self, params: Parameters<PathsParams>) -> Result<String, ErrorData> {
        let Parameters(args) = params;
        let config = effective_config(args.config, &self.config_override);
        run_blocking(move || ops::format(&args.paths, &args.exclude, config.as_deref(), false)).await
    }

    #[tool(
        description = "Lint files and apply available autofixes in place, then report remaining diagnostics as JSON. Writes files. Mirrors `poly lint --fix`.",
        annotations(read_only_hint = false, destructive_hint = true)
    )]
    async fn lint_fix(&self, params: Parameters<PathsParams>) -> Result<String, ErrorData> {
        let Parameters(args) = params;
        let config = effective_config(args.config, &self.config_override);
        run_blocking(move || ops::lint(&args.paths, &args.exclude, config.as_deref(), true)).await
    }

    #[tool(
        description = "Format files in place and report which files changed as JSON. Writes files. Mirrors `poly fmt --fix`.",
        annotations(read_only_hint = false, destructive_hint = true)
    )]
    async fn format_write(&self, params: Parameters<PathsParams>) -> Result<String, ErrorData> {
        let Parameters(args) = params;
        let config = effective_config(args.config, &self.config_override);
        run_blocking(move || ops::format(&args.paths, &args.exclude, config.as_deref(), true)).await
    }

    #[tool(
        description = "Report result-cache footprint (entry counts, sizes, format version) as JSON. Mirrors `poly cache stats`.",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    async fn cache_stats(&self) -> Result<String, ErrorData> {
        run_blocking(ops::cache_stats).await
    }

    #[tool(
        description = "Remove every cached entry and report freed bytes as JSON. Mirrors `poly cache clean`.",
        annotations(read_only_hint = false, destructive_hint = true)
    )]
    async fn cache_clean(&self) -> Result<String, ErrorData> {
        run_blocking(ops::cache_clean).await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for PolyMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("poly-mcp", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Universal zero-dependency linter & formatter. Tools mirror the `poly` CLI: \
                 lint / format_check / cache_stats are read-only; lint_fix / format_write / \
                 cache_clean write or mutate state. Every tool returns the same JSON as \
                 `poly … --format json`.",
            )
    }
}

impl PolyMcpServer {
    /// Names of the registered tools (introspection over the tool router).
    pub fn tool_names(&self) -> Vec<String> {
        self.tool_router
            .list_all()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect()
    }

    /// `(read_only_hint, destructive_hint)` for a named tool, if registered.
    pub fn tool_hints(&self, name: &str) -> Option<(Option<bool>, Option<bool>)> {
        self.tool_router.list_all().into_iter().find_map(|tool| {
            if tool.name == name {
                let annotations = tool.annotations.as_ref();
                Some((
                    annotations.and_then(|a| a.read_only_hint),
                    annotations.and_then(|a| a.destructive_hint),
                ))
            } else {
                None
            }
        })
    }
}
