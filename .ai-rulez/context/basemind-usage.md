---
priority: critical
---

# basemind-first: required tooling for this repo

This repo is indexed by **basemind**. Any agent working here should reach for basemind before
the naive equivalents: it returns paths, line numbers, and signatures — a fraction of the tokens
of reading source — and shares one index across the session. basemind first; shell/grep/git are
the fallback when a mode genuinely cannot answer. Do not re-read a file basemind has already
mapped, and run `admin` mode `rescan` after edits rather than reconnecting.

## The call shape

**Nine grouped tools, each taking a required `mode`**: `code`, `git`, `graph`, `memory`, `web`,
`agents`, `shell`, `workspace`, `admin`. **There are no flat per-operation tool names** — the
operation is the `mode` argument, so `search_symbols`, `workspace_grep`, `blame_file`,
`web_scrape`, `room_post`, `dm_send`, `shell_spawn` and the like are *not callable*.

When the MCP server is not wired into a session, every operation is available from the CLI as
`basemind <group> <mode>` (e.g. `basemind code symbols`, `basemind git blame_symbol`). A session
without the MCP tools is not a reason to skip basemind — it is a reason to shell out to it.

## 1. Code — instead of grep/ripgrep and opening files

`code` modes: `outline` (a file's symbols, signatures, imports; `l2: true` adds calls + docs),
`symbols` (where is X defined), `references` (every use site), `callers`, `grep` (full-text
across the repo), `find`, `definition`, `implementations`, `dependents`, `expand` (one symbol's
body), `semantic`.

Read an `outline` before opening a file, then fetch only the span you need.

## 2. Git — instead of naked `git`

`git` modes: `status`, `recent`, `search`, `touching`, `by_path`, `churn`, `diff`,
`diff_outline`, `blame`, `blame_symbol`, `symbol_history`.

Use these instead of `git log` / `git blame` / `git diff` / `git status`.

## 3. Graph — call chains and module structure

`graph` modes: `calls`, `neighbors`, `path`, `subgraph`, `communities`, `map`.

## 4. Memory and documents

`memory` modes: `put` / `get` / `list` / `search` for durable repo-scoped notes, and `documents`
for retrieval over indexed docs, PDFs and specs (RAG, keyword, entity/NER, summary) instead of
opening them by hand.

## 5. Web — researching an upstream crate

`web` modes: `scrape`, `crawl`, `map`. Use when confirming what an upstream library externalizes
before wrapping it — e.g. checking `ruff`, `oxc`, `taplo`, `sqruff`, or
`tree-sitter-language-pack` before writing a backend against it.

## 6. Agents, shell, workspace — coordinating with peers

Coordination runs over **threads**, not a global room, through the `agents` tool. See the
`agent-comms` rule for the full protocol; in short: `thread_list` / `inbox` to see what has been
said, `message` to read a body, `post` to say something, `thread_start` to open a thread
(addressed by at least two of `subject` / `path` / `members`), `list` to discover peers. There is
no direct-message operation — a private hand-off is a two-member thread. `as_agent` is a
**parameter** passed on each call, not a tool.

`shell` modes `spawn` / `send` / `capture` / `kill` / `list` / `broadcast` drive subagents.
`workspace` modes `workspaces` / `worktrees` / `branches` / `claim` show who already owns a tree
before you edit a shared checkout.
