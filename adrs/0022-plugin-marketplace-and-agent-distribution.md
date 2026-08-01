# 0022 — Plugin Marketplace and Agent Distribution

- Status: Accepted
- Date: 2026-08-01

## Context

poly's MCP server (`poly mcp`, ADR 0011/0021) gives an agent harness a stdio tool surface for
lint/format/cache/rules/config operations, but a harness must still be told to spawn it: a user
would have to hand-edit their Claude/Codex MCP config with the right command and args before any
agent could use poly at all. poly already generates its assistant-facing rules/context/skills
from `.ai-rulez/` (ai-rulez v4) for this repo's own contributor tooling; ai-rulez v4 added a
`[plugin]`/marketplace surface that fans the same source out to a distributable plugin bundle
(`.claude-plugin/`, `.codex-plugin/`) instead of just in-repo assistant config. Publishing poly's
own plugin closes the gap: `/plugin install` becomes the whole setup step for an agent to use
poly, no manual MCP config required.

## Decision

- **A `Goldziher/poly` ai-rulez marketplace, generated from `.ai-rulez/config.toml`
  `[plugin]`.** `.claude-plugin/plugin.json` + `.claude-plugin/marketplace.json` (Claude) and
  `.codex-plugin/plugin.json` (Codex) are generated output — never hand-edited — from the same
  `.ai-rulez/` source this repo already uses for its own CLAUDE.md/AGENTS.md. `presets =
  ["claude", "codex"]` scopes generation to those two harnesses; other ai-rulez presets (Cursor,
  Gemini, etc.) are pruned by `scripts/release-bump.sh` after generation so the committed tree
  stays claude/codex-scoped only.
- **`poly mcp` is registered assuming `poly` is already on `PATH` ("Option A"), not bundled
  with the plugin.** The plugin's `mcpServers` entry is `command = "poly"`, `args = ["mcp"]` —
  it invokes whatever `poly` binary the host resolves from `PATH`. This mirrors the existing,
  already-sanctioned distribution model (installer script, GitHub Action, Homebrew tap — see
  release-versioning) rather than introducing a second one: a user who has installed `poly` any
  of those ways already satisfies the plugin's only prerequisite. The alternative — bundling a
  prebuilt `poly` binary inside the plugin package, or having the plugin install `poly` on first
  use — was rejected (see Alternatives): it would require the plugin to carry or fetch
  platform-specific binaries itself, duplicating the installer/Action/Homebrew logic inside a
  fourth distribution path for marginal convenience.
- **Content scope: claude + codex harnesses only.** The plugin ships 5 skills (poly-overview,
  poly-lint-and-format, poly-tiers-and-scope, poly-mcp, poly-orchestrator) and 2 slash commands
  (poly-check, poly-fix) alongside the `poly` MCP server registration — teaching an agent what
  poly is, how its tiers work, how to drive it over MCP, and how to use it as the repo's single
  lint/format gate instead of invoking wrapped tools directly. No other harness-specific bundle
  (Cursor rules, a VS Code extension, etc.) is in scope for this decision.
- **Plugin version is lock-step with the workspace version.** `.ai-rulez/config.toml [plugin]
  version` always equals the root `Cargo.toml` `[workspace.package] version`, and the generated
  `.claude-plugin/plugin.json` / `.claude-plugin/marketplace.json` / `.codex-plugin/plugin.json`
  inherit it through generation — one more surface added to the existing lock-step versioning
  rule (release-versioning), not a separate version to track. `scripts/release-bump.sh` bumps
  `Cargo.toml` and `.ai-rulez/config.toml` together, regenerates the plugin outputs, and asserts
  the generated manifests carry the new version before allowing the bump to complete.
- **Install path.** Claude Code: `/plugin marketplace add Goldziher/poly` then `/plugin install
  poly@poly`. Codex: the equivalent plugin-marketplace flow for that client, pointed at
  `Goldziher/poly`, reading `.codex-plugin/plugin.json`. Both resolve to the same underlying
  `mcpServers` registration; no separate poly-specific installer is introduced for the plugin
  itself.

## Consequences

Positive:

- Installing poly's agent integration becomes a two-line marketplace command instead of manual
  MCP JSON editing, for both Claude Code and Codex.
- The plugin surface is generated from the same `.ai-rulez/` source already governing this repo's
  own CLAUDE.md/AGENTS.md, so there is one authoring surface, not two divergent ones.
- Lock-step versioning means a user can trust that the plugin they installed matches the `poly`
  binary version documented alongside it; `scripts/release-bump.sh` makes drift a release-time
  assertion failure rather than a silent possibility.
- No new distribution channel: the plugin's only prerequisite (`poly` on `PATH`) is exactly what
  the installer, GitHub Action, and Homebrew tap already provide.

Negative / risks:

- The plugin is inert without a separately installed `poly` binary — a user who installs the
  plugin before the binary gets a non-functional MCP server entry until they also run the
  installer. This is a discoverability cost of "Option A"; mitigated by documenting the
  prerequisite plainly (README, plugin description) rather than solved structurally.
- Generated plugin surfaces (`.claude-plugin/`, `.codex-plugin/`) are one more artifact class
  that must never be hand-edited — same discipline already required for CLAUDE.md/AGENTS.md, now
  extended to a third consumer (the plugin manifest itself).
- Scoping to claude + codex means any other harness a user relies on gets no equivalent
  distribution channel yet; extending presets is additive but each addition needs its own
  generation + pruning review.

## Alternatives considered

- **Bundle a prebuilt `poly` binary inside the plugin package ("Option B"):** rejected — it would
  mean shipping and maintaining platform-specific binaries through a fourth channel (installer,
  Action, Homebrew, plugin), each needing its own update cadence, when the plugin can instead
  assume any of the three existing channels already put `poly` on `PATH`.
- **Have the plugin install `poly` on first MCP connection (fetch + cache a binary at runtime):**
  rejected — reimplements the installer's download/verify logic inside the plugin runtime, a
  second copy of that logic to keep correct and secure (checksum verification, platform
  detection) for no benefit over telling the user to run the installer once.
- **Hand-maintain `.claude-plugin/marketplace.json` / `.claude-plugin/plugin.json` /
  `.codex-plugin/plugin.json` directly instead of generating them from `.ai-rulez/`:** rejected —
  poly already generates its assistant-facing docs from `.ai-rulez/`; a hand-maintained plugin
  manifest would drift from that source and duplicate the same name/description/version fields
  ai-rulez already owns.
- **A poly-specific marketplace name distinct from the repository name:** rejected — `Goldziher/poly`
  as both the GitHub repository and the marketplace source keeps `/plugin marketplace add
  Goldziher/poly` unsurprising; a separate marketplace identifier would add a name to remember
  for no benefit.
