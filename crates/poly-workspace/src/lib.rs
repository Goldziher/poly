//! Whole-project (workspace) lint orchestration, shared by the `poly` CLI and
//! the poly MCP server.
//!
//! `poly lint`'s per-file tier (native engines + catalog tools) cannot run
//! *whole-project* analysis tools — `cargo clippy`, `cargo-sort`, `cargo-deny`,
//! type checkers — because they need a whole-workspace view that does not fit
//! the per-file rayon unit (ADR 0014). Those tools already have a home as
//! whole-workspace hooks (ADR 0019). This crate bridges the two: it lowers the
//! parsed `[hooks]` config into the native [`poly_hooks`] model ([`lower`]),
//! reduces it to just the whole-project tool set, and runs it against the
//! **live worktree**, returning a structured [`WorkspaceLintOutcome`].
//!
//! The crate sits *above* [`poly_hooks`] (the pure, sync execution engine) and
//! *below* the application front ends. It deliberately does **not** load config
//! itself — the caller injects a resolved [`poly_config::PolyConfig`], so the
//! CLI keeps its git-remote `extends` resolver and the MCP server keeps its
//! network-free one, each without a dependency cycle.

pub mod lint;
pub mod lower;
mod support;

pub use lint::{
    WorkspaceLintOptions, WorkspaceLintOutcome, WorkspaceToolResult, planned_workspace_tool_ids,
    render_workspace_outcome, run_workspace_lint,
};
pub use support::{open_result_cache, sccache_settings, show_progress};
