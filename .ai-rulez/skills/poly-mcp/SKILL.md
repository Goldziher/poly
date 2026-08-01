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

## Parameters

The file-oriented tools take the same shape as the CLI:

- `paths` — files or directories to operate on (defaults to the repo root).
- `exclude` — glob(s) to skip on top of `.gitignore`.
- `config` — path to a specific `poly.toml`.

Results come back as structured JSON and compact TOON, matching the CLI `--format` output.
Treat the read-only tools as safe to call freely; gate the mutating tools behind explicit
intent since they change files.

(Exact tool names may shift slightly as the server stabilizes — the read-only/mutating split
and the params above are the stable contract.)
