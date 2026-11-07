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
  under it, so no other Markdown file in the repository is read. A `dir`, `paths` or `recursive` of
  the wrong type is an error naming the option, not a fall back to the default directory.
- **The tree is judged whole, whatever the run names.** For an engine that declares directories the
  host walks them itself instead of handing over the run's file set, so `poly lint docs/adr/0003-x.md`
  (what a pre-commit hook does) gives the same verdict as `poly lint .`, with the same cache key. The
  run's paths then only narrow what is reported: a finding is kept when it is in a named file or under
  a named directory. A run that names nothing in or above the declared directories does not read the
  tree, and an engine with nothing to judge (no served file and no side file) is not run.
- **The engine declares what it needs.** `Engine::project_inputs` (carried in the engine description
  and the component index, `WireEngineDesc.project_inputs`) names the gating option, the side files
  as (option, default path) and the directories with their options. The host reads that, not a
  table keyed by engine name, so a later engine such as skif declares its own inputs the same way.
  The field is optional on the wire, so the protocol version is unchanged.
- **`MADR42` through a listing.** For the declared directories (the subdirectories too under
  `recursive`) the host also passes the files the engine does not serve: every file type, names
  only, empty content, language `Other("listing")`, honoring `.gitignore`, the pruned directories,
  `[discovery] exclude` (and `--exclude`) and the generated-file rule. A file the engine serves is
  loaded with content, never listed, and the side files are not listed. The listing is part of the
  cache key, so a stray file appearing or going away re-runs the check. Whether an unserved file is
  generated is read from its first 16 KiB only, since a listing reads every file; the one divergence
  from the per-file rule is a generated unserved file whose front matter alone exceeds 16 KiB.
- **The lock is host input.** `adrlint.lock` (or the `lock` option's path, which must stay inside the
  project root) is read by the host and appended to the request as a file of language
  `Other("<file name>")`; the component takes the one such file that is not a listing entry as the
  manifest and reads nothing itself. A missing lock means no enforcement; a malformed one is an
  error. A lock that exists but cannot be used (a symlink, outside the project root, unreadable or
  not UTF-8) is an error too, reported like a wrong-typed option and failing the run (exit 2), since
  skipping it would silently turn `ADR50`/`ADR51` off. A configured directory or lock path that
  leaves the project root, or a directory that is a symlink, is likewise an error naming it.
- **What the listing and the log depend on.** `--exclude`, `[discovery] exclude` and `.gitignore`
  shape the set the host walks, so excluding an ADR file removes it from the log the rules judge;
  and a symlinked ADR file is a file the walk may not follow, so it is neither loaded nor listed.
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
