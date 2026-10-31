# 0037 — adrlint: MADR Architecture Decision Record Linting in poly

- Status: Accepted
- Date: 2026-10-31

## Context

poly lints Markdown with rumdl, which judges a document as Markdown. It has no opinion about what
the document *is*. An architecture decision record is a structured artifact — a MADR template
(ADR 0006's own decisions live in this repository's `adrs/`) whose value depends on section
structure, sequential numbering, stable file names and, once accepted, immutability. A generic
Markdown linter checks none of that.

`adrlint` is a maintainer-owned Rust linter for MADR records: one parse, MADR structural rules,
directory-log rules, a content-hash immutability manifest, and rumdl linked in-process for the
Markdown layer. It ships as a library (`adrlint::adr`, `adrlint::rules`,
`adrlint::diagnostics`), so poly can consume it without a subprocess.

The question is not whether to have MADR checking — it is how much of adrlint fits poly's engine
contract, and whether it runs unasked.

## Decision

**Add adrlint as a tier-1 Markdown lint backend** — `engines/adrlint.rs`, registered next to
`rumdl` for `Language::Markdown`, depending on `adrlint = "=0.4.0"` from crates.io.

### Scope: the document rules, not the log rules

poly's per-file engine is a pure function of `(bytes, path, config)`. That boundary decides which
adrlint rules the backend can own:

- **Run** — `MADR01`–`MADR08` (MADR structure), `MADR12b`/`MADR12d` (anchor resolution and
  absolute-path rejection), `MADR21` (front-matter YAML) and `MADR31` (list markers). These are
  functions of the ADR alone.
- **Stay in the `adrlint` CLI** — `MADR41`–`MADR44` (start number, gaps, duplicates, file names)
  need the whole directory; a per-file engine would report each finding once per ADR or attribute
  it to an arbitrary one. `ADR50`/`ADR51` need `adrlint.lock` and every accepted record. `MADR12a`
  and `MADR12c` stat link targets outside the ADR, and `MADR11` needs the network.

The last group is a cache-correctness decision, not a missing feature. The result cache keys on
the ADR's own bytes plus the engine version and config; a target file appearing later would leave
a cached "missing link" finding in place with nothing to invalidate it. Claiming to check links
while serving a stale verdict is worse than declining to check them. `adrlint lint` remains the
tool for the tree-wide rules, and `poly.toml`'s `enabled` key is not a substitute for it.

### Opt-in, off by default

**`[lint.markdown.adrlint] enabled = true` turns it on.** An existing ADR log may follow a
different template — this repository's own `adrs/` uses a lightweight MADR variant with
`## Context`, `## Decision` and `## Consequences`, not the `Context and Problem Statement` /
`Considered Options` / `Decision Outcome` set the rules require. Enabling enforcement unasked
would turn a passing run red on upgrade, which is exactly the false positive the project refuses
to ship. The engine is self-gating (`enabled` is read inside `lint`, and
`provides_language_lint` answers `false` when it is off) so it never claims Markdown lint
coverage it did not provide.

### Rule selection

`select`, `extend_select` and `ignore` follow ADR 0016. Codes match case-insensitively by prefix,
as adrlint's own `[rules]` table does (`MADR07` covers `MADR07a`/`MADR07b`). Severity overrides
use the existing universal `[lint.markdown.adrlint.rules.<id>] level` remap; the backend does not
reimplement it.

### Fixes

The one structural autofix adrlint applies — deepening a misplaced `## Consequences` /
`## Confirmation` heading to `###` — becomes a zero-width `Edit` inserting `#` at the heading's
indent, so `poly lint --fix` applies it through the normal atomic-edit path. adrlint's
*formatter* is rumdl, which poly already runs; the backend advertises `format: false` rather than
formatting the file a second time.

### Dependency and cache identity

`adrlint` is pinned at an exact version (`=0.4.0`), matching the policy for upstream internals
that are not semver-managed for policy purposes. `Engine::version()` embeds
`adrlint-0.4.0+poly1`; the suffix is bumped when the diagnostic or edit mapping changes, and the
version when the crate moves. `tests/version_audit.rs` asserts the first half against
`Cargo.lock`, and the registry's exhaustiveness guard requires the entry.

## Consequences

**Positive.** poly answers a question rumdl cannot: is this a well-formed MADR decision record?
The integration is in-process, zero-dependency, cached, fixable and configured through the same
`poly.toml` table shape as every other backend. A repository already using adrlint can move the
per-file checks into `poly lint` and keep the CLI for numbering and locking.

**Negative.** Two tools now judge an ADR, and the split is a boundary a reader has to learn. A
repository that wants full adrlint coverage still runs `adrlint lint` for the directory and lock
rules; poly cannot report a clean ADR log end-to-end. The split is documented in the backend
module, the README, the backend reference and this ADR, and it is enforced by the same
`enabled`-gated predicate that keeps the coverage claim honest.

**Risk.** adrlint and poly must agree on rumdl. Both link `rumdl 0.3`, cargo unifies it, and
poly's `RUMDL_VERSION` was bumped from `0.3.0` to `0.3.2` when adrlint raised the floor — the
version audit fails if the two drift silently.

## Alternatives considered

- **Shell out to the `adrlint` binary.** Violates the no-subprocess default (ADR 0002) and the
  per-file parallelism unit; also no cache key for a process.
- **Reimplement the MADR rules in poly.** Duplicates a maintained crate and guarantees drift in
  the rule set and diagnostics.
- **Run every rule, including directory and lock checks, in a per-file engine.** Reports log-level
  findings many times over, and cannot be cached safely.
- **Run adrlint as a whole-project hook.** The hook runner executes configured commands; a
  built-in backend is cacheable, configurable and visible in the backend table, and does not
  require a hook stage to be configured.
- **Default-on.** Rejected; see above. A repository opts in once its log conforms.
