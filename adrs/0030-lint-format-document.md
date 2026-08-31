# 0030 — The Lint/Format Document

- Status: Accepted
- Date: 2026-08-31

## Context

`poly lint --format json`/`toon` and `poly fmt --format json`/`toon` printed a bare **array** of
per-file records. That answers "what did you find", and nothing else. "What did you check" had to
be reconstructed by walking every record and testing two optional fields (`skipped`, `error`) — a
reconstruction no consumer actually performed, so a run that skipped everything was
indistinguishable from a clean one. ADR 0021 promoted `errors` to a top-level list inside the MCP
DTOs for exactly this reason on the MCP surface; the CLI's own `--format json` never got the same
treatment, so the two surfaces answered the coverage question differently — the MCP `lint` tool
could tell a caller a run failed to check something the CLI's own JSON output could not.

A second gap sat next to it. Two runs of an identical binary can enforce different rules: a
`poly.toml`, a `poly.local.toml`, a nested config, or an `extends` base can move underneath it, and
both report clean. Nothing in the payload said whether two clean reports were even comparable.

## Decision

**The array becomes an object: `{results, errors, skipped, summary, configs}`, on both the CLI and
the MCP server, in one change.**

- **`summary: RunSummary`** — `{checked, skipped, errored}` — is the run's own count of what it did.
  These numbers are **not** derivable from `results` and are stated because they cannot be: a file
  that was checked and found clean produces no record at all (`results` holds only files with
  something to report), and a file whose language nothing in the run lints can be **both** a result
  and a skip, since the cross-cutting backends (typos, ast-grep, the quality tier) still run over
  it. A consumer asking "was everything checked" compares `summary.checked` against the file count
  it expected — one comparison, not a scan.
- **`errors` and `skipped` are redundant with `results` by design.** An error or skip already
  appears as a `results` entry carrying no diagnostics; promoting it to its own top-level array is
  the same defect-closing move ADR 0021 made for MCP, generalized to the whole document: the defect
  is a consumer reading a clean-looking list and concluding the files are fine.
- **`configs: Vec<ConfigFingerprint>`**, each `{root, hash}` — `root` the nearest governing
  `poly.toml` directory (`null` under `--config`), `hash` a blake3 digest over the fully merged
  table — with each result's own `config: usize` field indexing into it (omitted when `0`, the
  common single-config case). This is what makes two clean reports comparable at all: a monorepo
  run legitimately carries several entries, each naming the directory it resolved from, so a
  difference between sibling packages is attributable rather than anomalous.
- **One type, `poly_core::report::{LintDocument, FormatDocument}`, built by both surfaces.**
  `LintDocument::from_run` / `FormatDocument::from_run` construct the object from a `LintRun` /
  `FormatRun`; the CLI's `--format json`/`toon` renderers and the MCP `lint`/`format_check`/
  `lint_fix`/`format_write` tools serialize the same value. The MCP DTOs that previously wrapped
  these types (`LintReport`/`FormatReport`, ADR 0021) are retired — they never diverged from what
  they wrapped, so the wrapper had nothing of its own to say, and returning the `poly-core` type
  directly is one fewer place the two surfaces could drift apart.
- **This is a breaking change to `poly lint`/`poly fmt --format json`/`toon`, taken deliberately and
  in one step for both surfaces, rather than only on MCP.** Fixing only the MCP shape (which
  already had the DTO indirection to change quietly) would have left the CLI's own JSON output
  telling a different, less honest story than the tool an agent calls through MCP — the exact
  divergence ADR 0021 was trying to close for one surface at a time. A consumer of either surface
  now answers "did this run check what I gave it" the same way.

## Consequences

Positive:

- A machine consumer can answer "was this run's coverage complete" from `summary` alone, without
  scanning `results` for absent entries.
- `configs` makes clean-report comparison honest in a monorepo: two runs reporting "no findings"
  are only the same claim when their `configs` entries match.
- One document type removes a whole class of CLI/MCP drift — the MCP DTOs cannot silently diverge
  from the CLI shape because they are no longer a separate type.

Negative / risks:

- **Every existing `--format json`/`toon` consumer that read the top level as an array must change**
  to read `results` (or the promoted `errors`/`skipped`) instead. No transition period or dual
  shape was offered — a versioned or content-negotiated array/object split was rejected as
  complexity that postpones the fix rather than avoiding it.
- `errors` and `skipped` duplicating `results` entries is deliberate redundancy, which costs
  payload size on a run with many skips or failures, in exchange for the property that a consumer
  reading only the top-level arrays cannot miss them.

## Alternatives considered

- **Fix only the MCP DTOs, leave the CLI's bare array alone:** rejected — it would leave the CLI
  and MCP surfaces answering the coverage question differently, which is the divergence this
  decision exists to close. ADR 0021 already took this narrower path once; repeating it here would
  leave the CLI permanently behind.
- **Keep the array and add a separate summary endpoint/flag:** rejected — a consumer that reads only
  the array (the common case, since it is the array's job to be the payload) would still see a
  clean-looking list on a run that skipped everything; the fix has to live in the one document a
  consumer actually reads.
- **Version the JSON shape (`--format json` stays an array, `--format json2` is the object):**
  rejected — it keeps the broken shape live indefinitely under its default name, which is worse
  than a clean, documented break at a stated version.
