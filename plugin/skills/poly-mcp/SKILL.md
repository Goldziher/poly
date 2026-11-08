---
description: The poly MCP server — the fourteen tools and which are read-only vs mutating, the paths/exclude/config/install/format params, component downloads, async Tasks, and the isError contract
name: poly-mcp
license: MIT
metadata:
    author: Goldziher
    repository: https://github.com/Goldziher/polylint
---

<!--
AI-RULEZ :: GENERATED FILE — DO NOT EDIT
Content-Hash: blake3:422fb9318db7715d1c421a5d935a7a0cdce9918dd0c8c78983801b3a3e2ff3b8
Source-Hash: blake3:ee175a52020d3c2a496fce6cf939500b91010d08bc917cda7ccb6774a6ec727d
Schema-Version: v1
-->

# poly MCP

`poly mcp` is a stdio MCP server exposing poly's lint/format/cache/rules/config surface as
tools. Prefer it over shelling out to the CLI when working through MCP — every tool returns
typed `structured_content` (with a declared output schema) plus one text block that mirrors
the CLI's `--format json`, or the compact TOON encoding when asked.

`poly mcp --config <PATH>` pins a fallback config file for requests that do not name one.

## Tool surface

Fourteen tools, no more. Read-only (never touch the tree):

- `lint` — run the linters and return diagnostics. Mirrors `poly lint`, per-file tier only
  (as `--workspace never`); the whole-project phase is the separate `workspace_lint` tool.
  May download a missing engine component (see "Components" below).
- `format_check` — report formatting drift without writing. Mirrors `poly fmt`. May download.
- `components` — the downloadable engine components and where each stands (`bundled`,
  `installed`, `not installed`, `unavailable`). Given `paths`, only the components they need,
  which are `missing`, and which a run would `would_download`. Never downloads. Mirrors `poly
  components list` / `status`.
- `cache_stats` — result-cache footprint (entries, bytes, format version) per namespace.
- `rules` — list every ast-grep rule a run would apply — poly's built-in pack plus the user
  rules from `[engines.astgrep] rule_dirs` — and optionally run their `*-test.yml` snippets. Mirrors `poly
  rules list` / `poly rules test`. Each rule carries its language, `source` (`builtin`/`user`),
  `default_severity`, the `severity` it reports at under the resolved config, and `enabled`.
- `config_show` — the merged effective configuration. Mirrors `poly config show`, and is
  **network-free**: remote `extends` bases are not fetched.
- `version` — which poly binary is serving this session (version, build id, channel,
  executable, pid, uptime), the full compiled-in engine → wrapped-tool-version map, and
  whether that executable is still the file on disk. It answers even when the binary has
  moved, which is what makes it the tool that explains why the others stopped.

Mutating (write to the tree):

- `lint_fix` — apply lint autofixes. Mirrors `poly lint --fix`. May download.
- `format_write` — format files in place. Mirrors `poly fmt --fix`. May download.
- `cache_clean` — clear the result cache and report freed bytes.
- `components_install` — download and verify components by `names` (components or presets),
  `languages` and/or `paths` (everything those paths need); `dry_run` reports the plan. Writes
  only the component cache. Mirrors `poly components install`.
- `components_uninstall` — remove components by `names`, and with `stale: true` the cached
  archives nothing references or used recently. Refuses a component loaded in this process
  (`refused`, and `isError`). Mirrors `poly components uninstall` / `prune`.

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

## Components

In a core build (every release binary except Linux musl) the engines are downloadable components;
the bundled Linux musl build links them in and nothing is ever missing. The result's `components` block (`used`, `installed_now`, `missing`) is
present only when a run needed one. A missing component that fails the run sets `isError`; one
skipped under `install: "never"` shows in `skipped` and `components.missing` without failing it.
Check `components.missing` before reading an empty `results` as clean.

`lint`, `format_check`, `lint_fix` and `format_write` are open-world: with `install: "config"`
(default, follows `[components] install`) or `"auto"` they download what the run needs before
running. `"never"` downloads nothing and skips the files a missing component would have
handled. A server started `--offline` never downloads whatever `install` says. `install`
governs engine components only; an opt-in recipe tool with `[tools.<name>] install = "auto"`
follows its own setting.

When the client declared the tasks capability and the run (or `components_install`) would
download, the call returns a Task: poll `tasks/get`. Its status message names the components;
there is no byte-level progress, and no `notifications/progress`. Otherwise the call blocks and
returns the result.

There is no `hooks` tool: `poly hooks` is CLI-only.

## Parameters

- `lint` / `format_check` / `lint_fix` / `format_write`: `paths` (files or directories;
  empty means the current directory), `exclude` (gitignore-style globs merged with
  `[discovery] exclude` — unanchored globs match at any depth), `config` (path to a
  `poly.toml`), `install` (`"config"` default, `"auto"` or `"never"`), `format` (`"json"`
  default, or `"toon"`).
- `components`: `paths`, `config`, `install` (the policy `would_download` is judged under),
  `format`.
- `components_install`: `names`, `languages`, `paths`, `config`, `dry_run`, `format`.
- `components_uninstall`: `names`, `stale`, `format`.
- `rules`: `dirs` (empty means `rule_dirs` from the config), `config`, `test` (bool),
  `format`.
- `config_show`: `config`, `format`.
- `cache_stats` / `cache_clean` / `version`: `format` only.
- `workspace_lint` / `workspace_lint_fix`: `config`, `format`, `jobs`, `no_cache` — no
  `paths`.

`format` selects only the paired **text** block; `structured_content` is always JSON.

Treat the read-only tools as safe to call freely; gate the mutating tools behind explicit
intent since they change files.

## Every result identifies the binary that answered

Each result carries a `poly` block (version, build id, channel, executable, pid, `flavor` —
`bundled`, `core` or `partial` — `components_lock` — a digest of the embedded component lock,
or null — and an `engines` digest — a blake3 hash over the sorted compiled-in engine name + version pairs,
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
