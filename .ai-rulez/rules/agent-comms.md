---
priority: high
---

# Agent comms & basemind-first

basemind is this repo's indexed context layer AND a multi-agent communication substrate. Two
standing directives for any agent working here.

**The MCP surface is nine grouped tools, each taking a required `mode`** — `code`, `git`, `graph`,
`memory`, `web`, `agents`, `shell`, `workspace`, `admin`. There are no flat per-operation tool
names (`search_symbols`, `room_post`, `web_scrape`, … are not callable); the operation is the
`mode` argument. When the MCP server is not wired into a session, the same operations are
available from the `basemind` CLI as `basemind <group> <mode>` (e.g. `basemind code symbols`,
`basemind agents post`).

**Prefer basemind — shell/grep/git are the fallback.** Reach for basemind before reading files,
before grep/ripgrep, and before naked `git`:

- `code` — modes `outline` (file structure; `l2: true` adds calls + docs), `symbols` (where is X
  defined), `references`, `callers`, `grep` (full-text across the repo), `find`, `definition`,
  `implementations`, `dependents`, `expand` (one symbol's body), `semantic`. Read an `outline`
  instead of opening a file, then fetch only the span you need.
- `git` — modes `status`, `recent`, `search`, `touching`, `by_path`, `churn`, `diff`,
  `diff_outline`, `blame`, `blame_symbol`, `symbol_history`. Use these instead of `git log` /
  `git blame` / `git diff` / `git status`.
- `graph` — modes `calls`, `neighbors`, `path`, `subgraph`, `communities`, `map` for call-chain
  and module-structure questions.
- `memory` — modes `put` / `get` / `list` / `search` for durable repo-scoped notes, and
  `documents` for retrieval over indexed docs/PDFs/specs (RAG, keywords, NER, summary).
- `web` — modes `scrape`, `crawl`, `map` when researching an upstream crate's API or docs.

They return paths, lines, and signatures — a fraction of the tokens of reading source. basemind
first; shell is the fallback. After editing code, run `admin` mode `rescan` rather than
reconnecting.

**Communicate with other agents.** You may be one of several agents working this repo at once.
Coordination runs over **threads** — scoped conversations, not a global chat room — all through
the `agents` tool:

- On start, check `agents` mode `thread_list` and mode `inbox` for what's been said. `history`
  and `inbox` return front-matter only (subject / from / id); call mode `message` with an id to
  read a body.
- Post concisely with mode `post` (`thread`, `subject`, `body`, optional `reply_to`) when you
  begin, finish, or hit a decision, and reply to messages about your work. Don't stay silent
  when collaborating.
- Open a thread with mode `thread_start`, addressed by **at least two** of `subject`, `path`
  (a glob like `src/**`), and `members`; fewer than two is rejected. A private hand-off to one
  peer is a two-member thread — there is no separate direct-message operation. Named members
  `join` (or are added with `add_member`) before they can post. `mode: "list"` discovers peers,
  `wait` blocks until a peer posts, `ack` clears read messages.
- `as_agent` is a **parameter**, not a tool: an orchestrator drives many named subagents by
  passing `as_agent: "<name>"` on every `agents` (and `workspace`) call, each with its own
  identity and inbox. Each subagent calls mode `register` once.
- To spawn and drive subagents you may also use the `shell` tool — modes `spawn`, `send`,
  `capture`, `kill`, `list`, `broadcast` — where applicable.
- Before editing a shared checkout, `workspace` modes `workspaces` / `worktrees` / `branches` /
  `claim` show who already owns the tree.

See the `multi-agent-room` skill for coordinating a team.
