// AI-RULEZ :: GENERATED FILE — DO NOT EDIT
// Content-Hash: blake3:2f9d5fc4d5d2b42544fd79fbf0b940465f6d70b056a4c3569e5c887613f23775
// Source-Hash: blake3:50c5aae2a919e52ef1216e799f10c78ca94cae41feba4aefe048570157d928cf
// Schema-Version: v1

/**
 * OpenCode v2 adapter for poly.
 *
 * This generated no-op keeps the plugin loadable without inventing runtime behavior.
 * To add OpenCode-specific tools or hooks:
 *
 * 1. Create .ai-rulez/opencode/index.js.
 * 2. Default-export Plugin.define({ id, setup }) from that source file.
 * 3. Run ai-rulez generate --plugin --dry-run.
 * 4. Run ai-rulez generate --plugin.
 *
 * Keep shared skills, commands, agents, and MCP configuration in their normal
 * .ai-rulez sources. Validate all external input and never interpolate untrusted
 * values into shell commands.
 */
import { Plugin } from "@opencode/plugin"

export default Plugin.define({
  id: "poly",
  async setup(ctx) {
    // Register hooks, transforms, tools, or subscriptions on ctx here.
  },
})
