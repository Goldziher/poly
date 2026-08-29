---
priority: medium
description: "The poly MCP server — the eleven tools and which are read-only vs mutating, the paths/exclude/config/format params, async Tasks for the whole-project phase, and the isError contract"
---

# poly MCP

`poly mcp` is a stdio MCP server exposing poly's lint/format/cache/rules/config surface as
tools. Prefer it over shelling out to the CLI when working through MCP — every tool returns
typed `structured_content` (with a declared output schema) plus one text block that mirrors
the CLI's `--format json`, or the compact TOON encoding when asked.

`poly mcp --config <PATH>` pins a fallback config file for requests that do not name one.

## Tool surface

Eleven tools, no more. Read-only (never touch the tree):

- `lint` — run the linters and return diagnostics. Mirrors `poly lint`.
- `format_check` — report formatting drift without writing. Mirrors `poly fmt --check`.
- `cache_stats` — result-cache footprint (entries, bytes, format version) per namespace.
- `rules` — list every ast-grep rule a run would apply — poly's built-in pack plus the user
  rules from `[rules] dirs` — and optionally run their `*-test.yml` snippets. Mirrors `poly
  rules list` / `poly rules test`. Each rule carries its language, `source` (`builtin`/`user`),
  `default_severity`, the `severity` it reports at under the resolved config, and `enabled`.
- `config_show` — the merged effective configuration. Mirrors `poly config show`, and is
  **network-free**: remote `extends` bases are not fetched.
- `version` — which poly binary is serving this session (version, build id, channel,
  executable, pid, uptime) and whether that executable is still the file on disk. It answers
  even when the binary has moved, which is what makes it the tool that explains why the
  others stopped.

Mutating (write to the tree):

- `lint_fix` — apply lint autofixes. Mirrors `poly lint --fix`.
- `format_write` — format files in place. Mirrors `poly fmt --fix`.
- `cache_clean` — clear the result cache and report freed bytes.

Whole-project (long-running):

- `workspace_lint` — the whole-project phase in check mode: `cargo clippy` / `cargo-sort` /
  `cargo-machete` / `cargo-deny` and configured inline whole-project jobs, over the whole
  repository (it takes no `paths`).
- `workspace_lint_fix` — the same phase in fix mode; writes files.

Both are exposed as async Tasks (SEP-2663): the call returns a task handle and the client
polls `tasks/get`, optionally `tasks/cancel`, with an unlimited TTL. A client that does not
declare the tasks capability gets a synchronous, blocking result from the same call instead,
so the tools work either way.

`workspace_lint` applies no fixes, but it is **not** in the never-touch-the-tree group: it
executes those tools against the live worktree, and their own side effects — a refreshed
lock file, a populated build or type-checker cache — are not poly's to control.

There is no `hooks` tool: `poly hooks` is CLI-only.

## Parameters

- `lint` / `format_check` / `lint_fix` / `format_write`: `paths` (files or directories;
  empty means the current directory), `exclude` (gitignore-style globs merged with
  `[discovery] exclude` — unanchored globs match at any depth), `config` (path to a
  `poly.toml`), `format` (`"json"` default, or `"toon"`).
- `rules`: `dirs` (empty means `[rules] dirs` from the config), `config`, `test` (bool),
  `format`.
- `config_show`: `config`, `format`.
- `cache_stats` / `cache_clean` / `version`: `format` only.
- `workspace_lint` / `workspace_lint_fix`: `config`, `format`, `jobs`, `no_cache` — no
  `paths`.

`format` selects only the paired **text** block; `structured_content` is always JSON.

Treat the read-only tools as safe to call freely; gate the mutating tools behind explicit
intent since they change files.

## Every result identifies the binary that answered

Each result carries a `poly` block (version, build id, channel, executable, pid) in
`structured_content` and in `_meta`, because an MCP caller has no `poly --version` to fall
back on. The server fingerprints its own executable at startup and re-checks it per request:
if the binary is replaced or deleted underneath a long-lived server, every tool but
`version` fails rather than answering with superseded behaviour.

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
