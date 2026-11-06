# 0041 — adrlint Tree-Wide Rules as a Project Check

- Status: Accepted
- Date: 2026-11-06
- Amends: ADR 0037
- Related: ADR 0039

## Context

ADR 0037 kept the adrlint rules that need the whole log out of poly: `MADR41`–`MADR44` (numbering,
gaps, duplicates, file names) and `ADR50`/`ADR51` (the immutability lock). A per-file engine would
report each of them once per ADR, and its result cache keys on one file's bytes, so a sibling ADR or
`adrlint.lock` changing would leave a stale verdict in place. ADR 0039 added the project-check
capability, a phase that hands an engine every file it serves with contents and caches the result on
all of them. adrlint 0.5.0 adds `rules::check_project` over an explicit file set and a parsed lock,
which reads no file.

## Decision

The `adrlint` engine in the `web` component declares `capabilities.project` and runs
`MADR41a`, `MADR41b`, `MADR42`, `MADR43`, `MADR44`, `ADR50` and `ADR51` through `check_project`.

- **The ADR log** is `dir` (default `docs/adr`, adrlint's default) or `paths`, with `recursive`,
  `start` and `allow_gaps` as in adrlint's `[adr]` table. The host hands over only the Markdown
  under it, so no other Markdown file in the repository is read.
- **The engine declares what it needs.** `Engine::project_inputs` (carried in the engine description
  and the component index, `WireEngineDesc.project_inputs`) names the gating option, the side files
  as (option, default path) and the directories with their options. The host reads that, not a
  table keyed by engine name, so a later engine such as skif declares its own inputs the same way.
  The field is optional on the wire, so the protocol version is unchanged.
- **`MADR42` through a listing.** For the declared directories (the subdirectories too under
  `recursive`) the host also passes the files the engine does not serve: every file type, names
  only, empty content, language `Other("listing")`, honoring `.gitignore`, the pruned directories,
  `[discovery] exclude` and the generated-file rule. The listing is part of the cache key, so a stray
  file appearing or going away re-runs the check.
- **The lock is host input.** `adrlint.lock` (or the `lock` option's path, which must stay inside the
  project root) is read by the host and appended to the request as a file of language
  `Other("adrlint.lock")`; the component reads nothing. A missing lock means no enforcement, a
  malformed one is an error.
- **The cache key needs nothing new.** The project-check key already folds in the digest of every
  file handed over, so editing, adding or removing an ADR, or changing the lock, re-runs the check.
- **Gated on `enabled`.** The engine declares `enabled` as its gate, as the per-file rules already
  require, so a project that never opted in pays nothing. `python-types` declares `backend` the
  same way.
- `MADR12a`/`MADR12c` (link targets exist) and `MADR11` (external links) stay out: they read the
  tree or the network.

## Consequences

`poly lint` reports the whole adrlint rule set except the link-existence and network rules. The
listing and the directory scope are bounded to the declared directories inside the project root;
the lock is read only from inside it.
