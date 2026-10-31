# Poly coding-agent plugins

The native ai-rulez authoring sources in `.ai-rulez/` generate eleven runtime profiles from
five public skills and two prompts. Contributor rules and maintainer agents stay in the
repository root and are excluded from this package. Install `poly` on PATH first: these
bundles register `poly mcp` or teach CLI workflows; they do not install the executable.

Claude Code installs the repository marketplace with `/plugin marketplace add Goldziher/poly`
and `/plugin install poly@poly`. Codex uses `codex plugin marketplace add Goldziher/poly`
then `codex plugin add poly@poly`. Both marketplace indexes point to this directory.

For local testing, clone the repository and use the generated files below:

| Runtime | Entry point or local installation |
| --- | --- |
| Claude Code | `.claude-plugin/plugin.json` |
| Cursor | `.cursor-plugin/plugin.json` |
| Codex | `plugin.json` and `.codex-plugin/plugin.json` |
| Gemini | `gemini extensions install ./plugin`; run from the repository root |
| Kimi | `kimi.plugin.json`; requires the Kimi Python CLI that supports plugins |
| OpenCode | `package.json` exposes `.opencode/plugins/poly.js` for its v2 API |
| Factory | `.factory-plugin/plugin.json` |
| Hermes | `.hermes/package/pyproject.toml` and `.hermes/plugins/poly/plugin.yaml` |
| Pi | `pi install ./plugin`; run from the repository root |
| Agent Plugins | `plugin.json` and `mcp.json`, spec 1.0 |
| Copilot | `plugin.json` and `mcp.json` in the shared Agent Plugins layout; prompt commands are not bundled |

Two runtimes also install from a registry. Each release publishes, through OIDC trusted
publishing:

- **`@goldziher/opencode-poly`** on npm — the OpenCode adapter plus the Pi skills and prompts.
  For OpenCode, `opencode plugin add @goldziher/opencode-poly`; for Pi,
  `pi install npm:@goldziher/opencode-poly`. Pi can also install from the Git repository with
  `pi install git:github.com/Goldziher/poly`; the private root package manifest points to this
  directory's skills and prompts. Pi uses its bash tool to call the installed CLI; it does not
  register MCP.
- **`poly-hermes-plugin`** on PyPI — the Hermes package.

Every runtime bundle and both marketplace indexes are also attached to each GitHub release as
`poly-plugins-v<version>.tar.gz`, with a `.sha256` beside it.

For Hermes, install the package into the Python environment Hermes runs in —
`uv pip install --python /path/to/hermes/venv/bin/python poly-hermes-plugin`, or
`./plugin/.hermes/package` from a repository checkout — then explicitly enable `poly` in that
Hermes installation's `config.yaml` under `plugins.enabled`. Skills are opt-in namespace loads,
such as `poly:poly-mcp`; they do not enter the flat system skill listing. The `poly_call` tool bridges
all eleven native MCP tools. Choose `list_tools` to inspect their request schemas, then pass
`tool_arguments` to the selected tool. Checking workspace lint can compile dependencies;
mutating tools (`lint_fix`, `format_write`, `cache_clean`, `workspace_lint_fix`) modify files
or caches and should only be used when the current task authorizes those changes.

Public skills install independently with `npx skills add Goldziher/poly --skill poly-mcp`.
Their source is `plugin/.ai-rulez/skills`; `scripts/sync_public_skills.py --check` verifies the
repository's `skills/` mirror, including resource bytes, namespaces and ownership.

Generation and verification use the latest ai-rulez release, in tasks and in CI.
Run `mise run plugin:generate`, `mise run plugin:verify` and `mise run plugin:test`. Native package
artifacts and marketplace indexes attach to tagged releases. The manual plugin artifact workflow packages
its selected checkout after checking that the core and plugin versions match an existing release tag. Marketplace indexes pin the
checkout's immutable commit, and the verified archive attaches to that release; the workflow
does not create a product tag or publish registry packages.
