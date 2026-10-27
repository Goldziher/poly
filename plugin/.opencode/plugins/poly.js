// AI-RULEZ :: GENERATED FILE — DO NOT EDIT
// Content-Hash: blake3:057210c54cbd2413e51ed54258c4b7b8b7de1ea9d8bf499281ac6874156d7f56
// Source-Hash: blake3:d6478f34c9eb0ddb169a467e10b085ba8afdfeef091e73780cd7e02a6877f882
// Schema-Version: v1

/**
 * OpenCode v2 adapter for poly.
 *
 * To add OpenCode-specific tools or hooks:
 *
 * 1. Create .ai-rulez/opencode/index.js.
 * 2. Default-export { id, setup(ctx) } from that source file. OpenCode v2 does not
 *    run v1 plugins (an exported function returning hooks).
 * 3. If the plugin bundles skills, commands, or agents, call
 *    registerBundledContent(ctx) from ../ai-rulez-content.js in setup.
 * 4. Run ai-rulez generate --plugin --dry-run, then ai-rulez generate --plugin.
 *
 * Keep shared skills, commands, agents, and MCP configuration in their normal
 * .ai-rulez sources. Validate all external input and never interpolate untrusted
 * values into shell commands.
 */
import { registerBundledContent } from "../ai-rulez-content.js"

/** @type {import("@opencode/plugin").Plugin.Plugin} */
export default {
  id: "poly",
  async setup(ctx) {
    await registerBundledContent(ctx)
  },
}
