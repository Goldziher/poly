---
priority: medium
description: "The poly MCP server — read-only vs mutating tools, the paths/exclude/config params, and JSON/TOON output mirroring the CLI"
---

# poly MCP

`poly mcp` is a stdio MCP server exposing poly's lint/format/cache surface as tools. Prefer
it over shelling out to the CLI when working through MCP — the outputs mirror the CLI's
`--format json`, and are also available as the compact TOON encoding.

## Tool surface

Read-only (never touch the tree):

- `lint` — run the linters and return diagnostics.
- `format_check` — report formatting drift without writing.
- `cache_stats` — result-cache statistics.
- `rules` — the effective rule set.
- `config_show` — the merged effective configuration.

Mutating (write to the tree):

- `lint_fix` — apply lint autofixes.
- `format_write` — format files in place.
- `cache_clean` — clear the result cache.
- `hooks` — run the configured hook stages.

Whole-project (long-running async Tasks — poll `tasks/get`):

- `workspace_lint` — the whole-project phase in check mode: `cargo clippy` / `cargo-sort` /
  `cargo-machete` / `cargo-deny` and configured whole-project jobs, over the whole repository
  (it takes no `paths`).
- `workspace_lint_fix` — the same phase in fix mode; writes files.

`workspace_lint` applies no fixes, but it is **not** in the never-touch-the-tree group: it
executes those tools against the live worktree, and their own side effects — a refreshed lock
file, a populated build or type-checker cache — are not poly's to control.

## Parameters

The file-oriented tools take the same shape as the CLI:

- `paths` — files or directories to operate on (defaults to the repo root).
- `exclude` — glob(s) to skip on top of `.gitignore`.
- `config` — path to a specific `poly.toml`.

Results come back as structured JSON and compact TOON, matching the CLI `--format` output.
Treat the read-only tools as safe to call freely; gate the mutating tools behind explicit
intent since they change files.

## Per-file outcomes: checked, skipped, error

`lint` / `lint_fix` / `format_check` / `format_write` results tell three per-file outcomes
apart, not two:

- **Checked** — the file has a `results` entry with no `skipped`/`error` set (diagnostics may
  still be empty; that's a clean file, not a missing one).
- **Skipped** (`skipped` field) — poly correctly declined the file (e.g. a template dialect no
  backend handles). Not a failure.
- **Errored** (`error` field) — poly failed to process the file (unreadable file, backend
  crash, bad engine config). Distinct from `skipped` on purpose: a skip is a deliberate
  decision, an error is poly failing on a file it accepted.

Each result also carries a run-level `errors` array — one entry per file poly failed on,
duplicating the `error`-carrying records in `results` so a caller can gate on "did the run fail
on anything" without scanning every record. When `errors` is non-empty, the tool result's
`CallToolResult.is_error` (`isError` over the wire) is set `true`.

**An MCP-driving agent must check `isError` before trusting any other part of the result.**
Before this shape existed, a file poly failed to process was absent from the output
entirely — indistinguishable from a file that was checked and found clean. An agent that
gated on "no findings in `results`" was reading a run that had silently failed to check some
files as a clean pass. Check `isError` (or the `errors` array) first; only then read
`results` for diagnostics.

(Exact tool names may shift slightly as the server stabilizes — the read-only/mutating split
and the params above are the stable contract.)
