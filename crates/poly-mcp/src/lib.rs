//! `poly-mcp` — an [MCP](https://modelcontextprotocol.io) server, over stdio,
//! that mirrors the `poly` CLI's lint/format/cache capabilities.
//!
//! The server exposes one tool per CLI capability (see [`server`]). Because MCP
//! tool annotations are static, read-only and mutating operations are split
//! into separate tools (`lint` vs `lint_fix`, `format_check` vs
//! `format_write`, `cache_stats` vs `cache_clean`) rather than gated behind a
//! boolean. Each tool runs the synchronous, rayon-driven `poly-core`
//! pipeline on a blocking task and returns the **same JSON** the CLI emits
//! under `--format json`.
//!
//! [`serve`] is a synchronous entrypoint: it builds a multi-threaded tokio
//! runtime internally and runs the stdio server to completion, so callers
//! (e.g. `poly mcp`) stay synchronous.

pub mod ops;
pub mod server;

use std::path::PathBuf;

use rmcp::ServiceExt;

pub use server::PolyMcpServer;

/// Run the MCP server over stdio until the client disconnects.
///
/// `config` optionally pins a config file used for every request that does not
/// name its own (the `poly mcp --config` passthrough). Builds its own tokio
/// runtime so the surrounding CLI can remain synchronous.
///
/// # Errors
///
/// Returns an error if the tokio runtime cannot be built or the stdio
/// transport fails to serve.
pub fn serve(config: Option<PathBuf>) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(serve_async(config))
}

/// Async body of [`serve`]: wire the server to the stdio transport and wait for
/// the session to end.
async fn serve_async(config: Option<PathBuf>) -> anyhow::Result<()> {
    let server = PolyMcpServer::new(config);
    let service = server.serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}
