import assert from "node:assert/strict";
import { test } from "node:test";
import plugin from "../../plugin/.opencode/plugins/poly.js";

async function register() {
  const out = { tools: [], skills: [], commands: [], mcp: {} };
  assert.equal(typeof plugin.setup, "function");
  await plugin.setup({
    location: { directory: process.cwd() },
    tool: { transform: (fn) => fn({ add: (tool) => out.tools.push(tool) }) },
    skill: { transform: (fn) => fn({ get: () => {}, add: (skill) => out.skills.push(skill) }) },
    command: { transform: (fn) => fn({ add: (command) => out.commands.push(command) }) },
    agent: { transform: (fn) => fn({ update: () => {} }) },
    mcp: {
      transform: (fn) =>
        fn({
          set: (name, config) => {
            out.mcp[name] = config;
          },
        }),
    },
  });
  return out;
}

test("registers bundled skills and MCP through the OpenCode v2 API", async () => {
  const out = await register();
  assert.equal(out.skills.length, 5);
  assert.equal(out.commands.length, 2);
  assert.equal(Object.keys(out.mcp).length, 1);
  assert.deepEqual(out.mcp["poly"].command, ["poly", "mcp"]);
  assert.equal(
    out.skills.every((skill) => skill.id.startsWith("poly/")),
    true,
  );
});
