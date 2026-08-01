# 0021 — MCP Structured Output, TOON, and Async Tasks

- Status: Accepted
- Date: 2026-08-01

## Context

The `poly mcp` server (introduced alongside the other `poly` subcommands, ADR 0011) exposed
`lint` / `format_check` / `lint_fix` / `format_write` / `cache_stats` / `cache_clean` as plain-text
JSON tool results: a single text content block holding the same array `poly lint --format json`
prints. That has three gaps against what `rmcp` 3.1.0 now offers and what a repo actually needs
from this server:

- **No schema, no structured content.** A client had to parse a raw JSON string out of a text
  block and had no machine-readable output schema to validate against or generate bindings from.
- **No compact text option.** JSON is the only text representation; poly's CLI already emits a
  compact [TOON](https://github.com/toon-format/spec) encoding (`--format toon`) that a
  token-constrained agent would prefer, and the server did not expose it.
- **No whole-project coverage, and no room for it.** `poly lint`'s whole-project phase (`cargo
  clippy` / `cargo-sort` / `cargo-machete` / `cargo-deny`, ADR 0019) can run for minutes. Every
  existing tool was a synchronous request/response call; wiring the whole-project phase in as one
  more synchronous tool would block the MCP connection for the duration of a `cargo clippy` run,
  with no way for a client to poll, show progress, or cancel.
- **Narrow tool surface.** The server had no read-only way to inspect the custom ast-grep rule
  set (`poly rules`) or the effective merged config (`poly config show`) — both already exist as
  CLI subcommands with no MCP equivalent.

## Decision

- **Structured content for every tool.** Each tool returns
  `rmcp::handler::server::wrapper::Json<T>` for a typed result DTO
  (`#[derive(Serialize, JsonSchema)]`, defined in `crates/poly-mcp/src/dto.rs`), so
  `CallToolResult.structured_content` carries the typed payload and the tool definition carries a
  derived output JSON schema (`#[tool(output_schema = …)]`). The lint/format DTOs
  (`LintReport`/`FormatReport`) wrap the `poly-core` report types verbatim so the schema matches
  the CLI's `--format json` shape exactly; the cache/rules/config/workspace DTOs are MCP-local
  because their CLI counterparts print prose rather than a serializable value.
- **A JSON or TOON text block alongside structured content, chosen per request.** Every
  path-taking tool accepts a `format: "json" | "toon"` parameter (`TextRepr`, default `json`).
  `structured_content` is always JSON regardless of `format` — `format` only selects the paired
  text block, reusing poly's existing `report::report_lint_toon` / `report_format_toon` (and a
  generic `serde_toon::to_string` for the MCP-local DTOs) rather than reimplementing a TOON writer.
  A client gets machine-readable JSON and a human/compact text view from one call.
- **Async Tasks for the whole-project phase.** `workspace_lint` and `workspace_lint_fix` — the
  multi-minute `cargo clippy`/`cargo-sort`/`cargo-machete`/`cargo-deny` phase — are exposed via
  `rmcp::task_manager::TaskManager` (SEP-2663 async Tasks): the call returns a task handle
  immediately and the client polls `tasks/get` (and may `tasks/cancel`), rather than holding the
  request open. TTL is unlimited (`with_ttl_ms(None)`) since the phase can legitimately run past
  the library's default 5-minute TTL. A client that does not declare the tasks capability gets a
  synchronous (blocking) result instead — the same `ops::workspace_lint` call, just awaited inline
  — so every client can use the tools regardless of capability negotiation. Because a `#[tool]`
  macro function cannot return a task handle, these two tools are registered by hand (`Tool::new`
  plus manual `list_tools`/`get_tool`/`call_tool` dispatch in `ServerHandler`) instead of via
  `#[tool_router]`, and reuse `poly_workspace::run_workspace_lint` — the whole-project lint
  orchestration extracted out of `poly-cli` into the shared `poly-workspace` crate so the CLI and
  the MCP server run one implementation instead of two — so behavior stays identical to `poly
  lint`'s whole-project phase.
- **Broadened, read-only-first tool surface.** `rules` (list, and optionally test, the custom
  ast-grep rule packs — mirrors `poly rules list`/`poly rules test`) and `config_show` (the
  effective merged config — mirrors `poly config show`, network-free) are added as read-only
  tools alongside the existing `lint` / `format_check` / `cache_stats`. `config_show` documents
  the same network-free limitation as the CLI's non-remote path (ADR 0020): a repo whose config
  `extends` a remote git base must be served via the `poly` CLI, not the MCP server, since the git
  resolver lives in `poly-cli` and the server cannot depend on it without a cycle.
- **Annotations are static per tool, so read-only and mutating variants stay separate tools**
  rather than one tool gated behind a write-enable flag: `lint` / `format_check` / `cache_stats` /
  `rules` / `config_show` / `workspace_lint` set `read_only_hint = true`, `destructive_hint =
  false`, `idempotent_hint = true`, `open_world_hint = false`; `lint_fix` / `format_write` /
  `cache_clean` / `workspace_lint_fix` set `read_only_hint = false`, `destructive_hint = true`,
  `open_world_hint = false`. A client can decide whether to call a tool, or ask for confirmation
  first, from the annotation alone.
- **Transport stays stdio-only.** No HTTP, SSE, or OAuth transport is added. `poly mcp` is
  designed to be spawned by a host process (an editor, an agent harness, the poly Claude/Codex
  plugin) over stdio, matching the sanctioned deployment model (ADR 0011) and the plugin's own
  `mcpServers` registration (see ADR 0022). This is explicitly out of scope for this decision,
  not deferred for a technical reason — introducing network transports raises a distinct set of
  auth/exposure questions that stdio-only sidesteps entirely.

## Consequences

Positive:

- A client gets a real JSON schema per tool and a typed payload to validate against, instead of
  parsing an untyped JSON string out of a text block.
- The compact TOON option lets a token-constrained agent ask for the same data in less space,
  reusing poly's existing TOON writers rather than adding a second implementation to maintain.
- The whole-project phase becomes usable over MCP at all — previously it had no MCP surface
  because a synchronous call would block for however long `cargo clippy` takes. Async Tasks (with
  a synchronous fallback) make it available to every client, task-aware or not.
- `rules` and `config_show` close a coverage gap: an agent driving poly over MCP can now inspect
  the rule set and effective config the same way it can over the CLI.

Negative / risks:

- Two tools (`workspace_lint`/`workspace_lint_fix`) can no longer use the `#[tool_router]` macro
  and are registered by hand, which is more code to keep in sync with the router's tool listing
  (`list_tools`/`get_tool` merge both sources) whenever the router gains a new macro tool.
- A synchronous fallback for non-task clients means the same operation has two code paths (Task
  vs. blocking call) to keep behaviorally identical; both call the same `ops::workspace_lint`
  function to minimize drift.
- `structured_content` being unconditionally JSON while the text block can be TOON means a client
  that only reads the text block and expects JSON must check `format` first — documented in the
  tool description and this ADR, not enforced by the type system.

## Alternatives considered

- **A single tool with a `write: bool` flag instead of separate read-only/mutating tools:**
  rejected — MCP annotations are static per tool, not conditional on a parameter, so a single tool
  could not honestly declare `destructive_hint` for both its read and write modes. Splitting into
  named tools (as poly already does: `lint`/`lint_fix`, `format_check`/`format_write`) keeps the
  annotation honest.
- **Reimplement TOON encoding in `poly-mcp` rather than reusing `poly-core::report`:** rejected —
  it would drift from the CLI's TOON output and duplicate a writer poly already ships; the DTOs
  either wrap the CLI report types directly or fall back to a generic `serde_toon` encoding for
  MCP-local shapes.
- **Keep the whole-project phase out of MCP entirely:** rejected — it is real coverage the CLI
  has (`poly lint`'s whole-project phase, ADR 0019) that an MCP-driven agent would otherwise have
  to invoke by shelling out to `poly` itself, defeating the point of an MCP server.
- **HTTP/SSE transport for the MCP server:** rejected for this decision — stdio matches every
  sanctioned deployment path (plugin, editor, agent harness) and avoids a distinct security
  surface (auth, exposure, CORS) that a network transport would require designing. Revisit only if
  a concrete deployment need for a networked poly-mcp server emerges.
