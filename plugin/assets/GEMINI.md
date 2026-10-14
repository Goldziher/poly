# Using Poly

Install the `poly` binary and put it on PATH before using this plugin. The plugin registers
`poly mcp`; it does not install or bundle the CLI. Start with `poly-overview`, then use
`poly-lint-and-format` for checking or applying changes and `poly-orchestrator` for configuration.

Use `poly lint --no-workspace .` when a check must avoid executing project toolchains.
Whole-project lint can build dependencies and refresh toolchain caches even without `--fix`.
Request approval for modifying files when the current task only authorizes inspection.

Pi bundles skills and prompts and uses the CLI through its bash tool. Hermes provides
`poly_call`: choose `list_tools` to inspect native MCP schemas before making a request.
Hermes plugin skills are explicit namespaced loads, such as `poly:poly-mcp`.
