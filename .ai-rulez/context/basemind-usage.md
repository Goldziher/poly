---
priority: critical
---

# basemind-first: required tooling for this repo

This repo is indexed by **basemind** (served over MCP). Any agent working here **MUST** use
basemind's tools instead of the naive equivalents. They return paths, line numbers, and
signatures — a fraction of the tokens of reading source — and they share one index across the
session. basemind first; shell/grep/git are the fallback only when a tool genuinely cannot
answer. Do not re-read a file basemind has already mapped; `rescan` after edits instead of
reconnecting.

## 1. Code search — NOT grep/ripgrep/reading files

- `search_symbols` — "where is X defined?" Use instead of grepping for a definition.
- `outline` — the shape of a file (symbols, signatures, imports); add `l2: true` for calls +
  docs. `outline` a file before you open it, then read only the span you need.
- `find_references` — every use site of a name. Use instead of grepping call sites.
- `find_callers` — callers of a specific definition.
- `workspace_grep` — full-text search across the repo. Use instead of shelling out to grep /
  ripgrep.

## 2. Git history — NOT naked `git`

- `recent_changes` — what changed recently.
- `blame_file` / `blame_symbol` — who last touched a file or symbol (symbol-resolution blame).
- `diff_file` / `diff_outline` — diffs at file or symbol granularity.
- `commits_touching` — history for a path/symbol.

Use these instead of `git log` / `git blame` / `git diff`.

## 3. Crawling & document intelligence — when researching crate APIs / docs

- `web_scrape` / `web_crawl` / `web_map` — scrape a page, crawl a site, or fetch a sitemap when
  researching an upstream crate's API or documentation (e.g. confirming what `ruff`, `oxc`,
  `taplo`, `sqruff`, or `tree-sitter-language-pack` externalize before wrapping or vendoring).
- `search_documents` and the documents pipeline (RAG, keyword + entity/NER, summary) — extract
  and search over docs/PDFs/specs in the repo instead of opening them by hand.

## 4. Semantic + free-text search

- `search_documents` — semantic / RAG search across indexed documents.
- `search_symbols` — free-text + symbol search across code.

## 5. Spawning subagents via basemind shells

- `shell_spawn` / `shell_send` / `shell_broadcast` / `shell_list` / `shell_capture` /
  `shell_kill` — spawn and drive subagents (e.g. one per backend) where applicable, in addition
  to `as_agent` / `dm_send`.

## 6. Agent communication — coordinate with peers

- `agent_list` — discover other agents on the repo.
- `room_list` / `room_history` / `inbox_read` / `message_get` — read what's been said
  (`room_history` / `inbox_read` return front-matter only; call `message_get` with an id for a
  body).
- `room_post` / `dm_send` — post status when you begin, finish, or hit a decision; DM a
  specific peer. Don't stay silent when collaborating.
</content>
