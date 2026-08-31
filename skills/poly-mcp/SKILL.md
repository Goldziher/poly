---
priority: medium
description: "The poly MCP server — the eleven tools and which are read-only vs mutating, the paths/exclude/config/format params, async Tasks for the whole-project phase, and the isError contract"
---

<!--
AI-RULEZ :: GENERATED FILE — DO NOT EDIT
Content-Hash: blake3:2ceee64a619ac4d86ebeb8badbeeb50cd33bc1c277ba3d00765558a1202b7194
Source-Hash: blake3:0adf16213f8c0f77e494fd60f9136dd61420c257778d9df38c0bbca19b5f83e9
Schema-Version: v1
-->

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
  executable, pid, uptime), the full compiled-in engine → wrapped-tool-version map, and
  whether that executable is still the file on disk. It answers even when the binary has
  moved, which is what makes it the tool that explains why the others stopped.

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

Each result carries a `poly` block (version, build id, channel, executable, pid, and an
`engines` digest — a blake3 hash over the sorted compiled-in engine name + version pairs,
excluding native-toolchain and catalog engines since those depend on the host and on config)
in `structured_content` and in `_meta`, because an MCP caller has no `poly --version` to fall
back on. The digest is what distinguishes two builds sharing one version number (a `dev`
build, or two builds of one tag) when their wrapped crates differ; the full engine map is on
the `version` tool. The server fingerprints its own executable at startup and re-checks it per
request: if the binary is replaced or deleted underneath a long-lived server, every tool but
`version` fails rather than answering with superseded behaviour.

## Per-file outcomes: checked, skipped, error

The result object is `{results, errors, skipped, summary, configs}`, not a bare array.
`summary` is `{checked, skipped, errored}` — the run's own count of what it did, and the field
to gate on. It cannot be derived from `results`: `results` holds only files with something to
report, so a file that was checked and found clean produces **no** `results` entry at all — a
present entry does not mean checked, either, since `results` also carries a synthetic entry
(empty `diagnostics`, `skipped` or `error` set) for every file the run declined or failed on.
The skipped/error sets and `results` overlap on purpose:

- **Checked** — counted in `summary.checked`. Not derivable from `results`.
- **Skipped** (`skipped` field, and the top-level `skipped` array) — poly correctly declined the
  file (e.g. a template dialect no backend handles). Not a failure.
- **Errored** (`error` field, and the top-level `errors` array) — poly failed to process the
  file (unreadable file, backend crash, bad engine config). Distinct from `skipped` on purpose:
  a skip is a deliberate decision, an error is poly failing on a file it accepted.

The top-level `errors` array duplicates the `error`-carrying records in `results` so a caller
can gate on "did the run fail on anything" without scanning every record. When `errors` is
non-empty, the tool result's `CallToolResult.is_error` (`isError` over the wire) is set `true`.

**An MCP-driving agent must check `isError` (or `summary.errored`) before trusting any other
part of the result.** Before this shape existed, a file poly failed to process was absent from
the output entirely — indistinguishable from a file that was checked and found clean. An agent
that gated on "no findings in `results`" was reading a run that had silently failed to check
some files as a clean pass. Check `isError` first; only then read `summary` for coverage and
`results` for diagnostics.
