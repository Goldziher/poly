# ast-grep pack rule audit — JS/TS and Python (2026-08-31)

The evidence behind poly's answer to issues #23 and #24, kept because a rule's
default severity is only as defensible as the measurement under it, and a
measurement nobody can find gets redone badly or not at all.

Five candidate rules were drafted, validated with `poly rules test` (161
assertions, 0 failures), and measured against a ten-repository corpus at pinned
SHAs. Four of the ten have LLM-authored histories by commit trailer and two are
human-authored controls; the corpus is now `scripts/harden/repos.c.tsv`'s C2
section, so the numbers below can be reproduced and the selection procedure
re-run.

**Outcome: none of the five shipped.** Four were dropped outright, one is kept
as a draft and stays `off`, and the strongest answer to #23 turned out not to
be a pack rule at all — oxlint already implements the rule that mattered, and
poly could not reach it. The reasoning per rule is below; the language is the
proposal's, written before the decision, and left that way deliberately.

| rule | languages | raw findings | per 1000 files | hand-read FP | verdict |
|---|---|---|---|---|---|
| `test-without-assertion` | javascript, typescript, tsx | 234 | 21.8 | 12/20 (60%) | **keep, `off`** + `**/*.test-d.ts` exclusion |
| `placeholder-implementation` | javascript, typescript, tsx | 186 | 17.3 | 20/20 in test paths, 15/20 residual | **drop** |
| `swallowed-rejection` | javascript, typescript, tsx | 594 | 55.2 | 19/20 (95%) | **drop** |
| `test-without-assertion` | python | 141 | 64.8 | 16/20 (80%) | **drop** |
| `placeholder-implementation` | python | 47 | 21.6 | 14/14 residual (100%) | **drop** |
| `todo-marker` (existing, python) | python | 82 | 38.5 | 0/20 (0%) | **keep `off`** |

The drafts themselves are **not** in the repository. Four were dropped and the
fifth is not shipping, so committing them would leave five rules nobody runs
sitting next to twenty-six that everybody does — and this repo deletes dead code
rather than parking it. Each rule is described below precisely enough to
re-author, and `git log` for this document points at the work that produced
them. What is worth keeping is the measurement, not the YAML.

---

## Question 1 — the JSX/TSX family

**Settled empirically. One rule file covers exactly one language, the family is
three grammars rather than four, and `.jsx` is unreachable today.**

The pack loader (`engines/astgrep/pack.rs`) groups parsed rules into a
`RuleMap` keyed by `rule.language.name()`, and `mod.rs`'s `resolve_rules` looks
that map up by `src.language.id()`. `Language::id()` returns four distinct
strings for the family — `"javascript"`, `"jsx"`, `"typescript"`, `"tsx"` — so
a rule file's single `language:` field selects exactly one key. Verified: a
`language: typescript` probe rule fires on `.ts` and not on `.tsx`; adding a
`language: tsx` copy makes `.tsx` fire.

Sharing one *compiled* rule across the family is not merely unimplemented, it
is unsound. `TslpLanguage::kind_to_id` resolves a `kind:` against
`get_language(self.name)`, and tree-sitter assigns node-kind ids per grammar;
the typescript and tsx grammars are separate `tree_sitter::Language` objects
with independently numbered kinds. A rule compiled against typescript cannot be
matched against a tsx parse tree.

**`.jsx` is worse than duplicated — it is uncovered.**
`tree-sitter-language-pack` 1.15.12 ships no `jsx` grammar (371 grammars;
`javascript`, `typescript`, `tsx` are present, `jsx` is not). Consequences,
both verified:

- A pack file declaring `language: jsx` fails `TslpLanguage`'s `Deserialize`
  (`unknown language 'jsx': not found in tree-sitter-language-pack`). In the
  pack that is a **panic at first `builtin_pack()` call**, since the loader
  `unwrap_or_else(|error| panic!(..))`s. As a user rule it aborts the whole
  rule-dir load — one bad file took down the other two rules in the same
  directory in the probe.
- `Engine::lint` bails at `TslpLanguage::new("jsx")` returning `None`, so
  **ast-grep never runs on a `.jsx` file at all** — not the pack, not user
  rules. `provides_language_lint` agrees, so this is silent rather than
  reported as a skip.

**Proposal.** Do not write sixteen files, and do not write nine either. Two
changes, both in `pack.rs`, neither touching the rule schema:

1. Give `PACK_RULES` entries an optional family: instead of one
   `include_str!` per `(rule, language)`, hold `(&str, &[&str])` and, for each
   language in the list, substitute the `language:` line and parse the text
   again. Re-parsing is the point — each family member gets its own matcher
   compiled against its own grammar, which is the only correct way — and it
   costs nothing, since the pack is built once into a `OnceLock` from
   compile-time constants. One authored file per rule, three `RuleConfig`s.
2. Route `Language::Jsx` to the `javascript` grammar for ast-grep. TSLP's
   `javascript` grammar parses JSX (tree-sitter-javascript has always included
   it), so the fix is a `Language::id()` → grammar-name mapping in
   `engines/astgrep/` rather than the identity it uses today. That is a
   prerequisite for JSX coverage of *any* rule, and it also closes the
   silent-no-coverage hole for the user-rule path.

Until (1) lands, the drafts here are three physical copies per rule, generated
from a single source, since the three grammars need three files.

## Question 2 — test-context detection

poly has two mechanisms and the two candidates need different ones. Neither
needs `NOISY_PATH_EXCLUSIONS`.

- **`test-without-assertion` needs a *syntactic* predicate, inverted — and one
  already exists in the language.** In JavaScript it is the callee: the rule
  matches a `call_expression` whose function is `it` / `test` (or `.only` /
  `.concurrent` / `.sequential` / `.each`) *and* which is passed a function
  argument. That is the framework's own definition of a test, so the rule
  applies inside test code by construction, and outside it matches nothing.
  In Python it is the name: pytest collects functions named `test_*` wherever
  they live, so `has: { field: name, regex: '^test_' }` is the predicate, and
  it covers `unittest` methods for free. Nothing here reads a path, so neither
  rule needs a `NOISY_PATH_EXCLUSIONS` entry for *test* detection.

  Both predicates needed *negative* carve-outs found by measurement, not by
  design: skip `it.skip` / `it.todo` and a bare `it("pending")` with no
  callback; in Python skip a `def test_*` nested inside another function (a
  callback named `test_hook`), one decorated `@pytest.fixture` / `@task` /
  `@before_*`, and require the underscore so a CLI command called `test` is not
  collected. Those three carve-outs alone removed 105 of 246 Python findings.

- **`test-without-assertion` (JS/TS) does need a path exclusion, for something
  else entirely**: vitest *type test* files, `*.test-d.ts`, where the compiler
  is the assertion and no runtime `expect` is expected. 147 of the rule's 234
  findings (63%) are there — one repository, vercel-ai, supplies all of them,
  and its five largest files supply 103. That is a `NOISY_PATH_EXCLUSIONS`
  entry (`**/*.test-d.ts`) of exactly the `frb_generated.rs` kind.

- **`placeholder-implementation` and `swallowed-rejection` want the *path*
  mechanism** — and it is not enough to save either. 85% of
  `placeholder-implementation`'s JS findings and 70% of its Python findings sit
  in test paths (mocks stubbing interface members), which a `**/test/**` glob
  would remove; but the non-test residual measured 75% and 100% FP
  respectively, so exclusion only shrinks a rule that was wrong anyway.
  `swallowed-rejection` has only 15% of its findings in test paths, so the
  mechanism does not apply at all.

---

## Per-rule findings

### `test-without-assertion` — javascript, typescript, tsx — **keep, `off`**

Catches an `it(..)` / `test(..)` whose callback reaches no assertion:
no `expect`, no `assert*`, no chai `.should`, no ava `t.is`-family call, no
jest/vitest matcher, no sinon `.calledWith`, no snapshot, no `throw`, and no
helper named `assert*` / `check*` / `verify*` / `expect*` / `test*` / `*Test`.

**Why oxlint does not already cover it.** oxlint *implements* the check —
`jest/expect-expect` and `vitest/expect-expect`, both `correctness` — but poly
cannot reach it. `ConfigStoreBuilder::default()`'s plugin set is
`LintPlugins::UNICORN | TYPESCRIPT | OXC` (`oxc_linter/src/config/plugins.rs`);
`jest` and `vitest` are not in it, poly's `DEFAULT_LINT_FILTERS` add categories
and named rules but never a plugin, and there is no config key for one.
Verified three ways: a vitest test file with an assertion-free `it` reports
nothing from oxlint; `[lint.typescript.oxc] extend_select =
["vitest/expect-expect", "jest/expect-expect"]` still reports nothing; and the
default plugin set is read off the pinned oxc source. The other rules #23 ruled
out were re-checked and stay ruled out.

```text

opencode: 12 findings, 1948 js files, 6.2/1000 js files
      4  packages/opencode/test/lib/effect.ts
      2  packages/core/test/lib/effect.ts
      2  packages/llm/test/lib/effect.ts
      1  packages/core/test/database-migration.test.ts
      1  packages/core/test/effect/layer-node/layer-node-types.test.ts
      1  packages/http-recorder/test/record-replay.test.ts
      1  packages/httpapi-codegen/test/effect.ts

stagehand: 1 findings, 371 js files, 2.7/1000 js files
      1  packages/protocol/tests/protocol/generated-notifications.test.ts

copilot-chat: 52 findings, 2173 js files, 23.9/1000 js files
      8  src/extension/githubMcp/test/node/githubMcpDefinitionProvider.spec.ts
      7  src/platform/notebook/test/node/alternativeContent.spec.ts
      6  src/platform/otel/common/test/noopOtelService.spec.ts
      4  src/extension/tools/node/test/findTextInFilesTool.spec.tsx
      3  src/extension/prompts/node/test/fixtures/strings.test-example.3.summarized.ts
      3  src/extension/tools/node/test/findFiles.spec.tsx
      2  src/extension/chatSessions/copilotcli/vscode-node/test/diffCommands.spec.ts
      2  src/extension/chatSessions/vscode-node/test/copilotCLISDKUpgrade.spec.ts
      2  src/extension/prompts/node/test/fixtures/strings.test-example.2.summarized.ts
      2  src/extension/prompts/node/test/fixtures/strings.test-example.summarized.ts

vercel-ai: 150 findings, 4051 js files, 37.0/1000 js files
     30  packages/ai/src/generate-text/generate-text.test-d.ts
     30  packages/ai/src/generate-text/stream-text.test-d.ts
     21  packages/ai/src/agent/tool-loop-agent.test-d.ts
     12  packages/xai/src/xai-video-options.test-d.ts
     10  packages/ai/src/generate-text/tool-approval-configuration.test-d.ts
      7  packages/ai/src/registry/provider-registry.test-d.ts
      7  packages/provider-utils/src/types/tool.test-d.ts
      6  packages/harness/src/v1/harness-v1-network-sandbox-session.test-d.ts
      4  packages/ai/src/ui/ui-messages.test-d.ts
      4  packages/harness-acp/src/acp-harness.test-d.ts

astro: 18 findings, 1374 js files, 13.1/1000 js files
      3  packages/astro/test/types/define-config.ts
      3  packages/astro/test/units/cache/noop.test.ts
      2  packages/astro/test/build-readonly-file.test.ts
      1  .agents/evals/skills.eval.ts
      1  packages/astro/e2e/astro-component.test.ts
      1  packages/astro/test/0-css.test.ts
      1  packages/astro/test/astro-i18n-client.test.ts
      1  packages/astro/test/ssr-preview.test.ts
      1  packages/astro/test/types/astro-i18n-virtual-module.ts
      1  packages/integrations/markdoc/test/render.test.ts

openhands: 1 findings, 836 js files, 1.2/1000 js files
      1  __tests__/hooks/use-terminal.test.tsx

aider: 0 findings, 13 js files, 0.0/1000 js files

pydantic-ai: 0 findings, 1 js files, 0.0/1000 js files

crewai: 0 findings, 3 js files, 0.0/1000 js files
```

Hand-read of 20 from the 87-finding non-`test-d` residual: **12/20 false
positives (60%)** — six deliberate "does not throw / handles X gracefully"
tests (several carrying a literal `// Should not throw` comment), two
elided-source prompt fixtures under `test/fixtures/`, two whose assertion is
`await`ing an event promise that would time out, one assertion in a helper the
vocabulary does not name, one type test under `test/types/`. The eight true
positives are real: a VS Code tool `invoke`d with nothing checked, a `Debouncer`
exercised and not measured, a `noopOtelService` test titled "flush resolves
immediately" that never checks timing.

Three rounds of narrowing took the raw count from 928 to 234; the note records
what each round added. **Recommend shipping `off`** with the `**/*.test-d.ts`
exclusion — 60% is double the 30% the Rust sibling ships `off` at, so it is a
rule to opt into, not a default. **The better answer to #23 is to give the oxc
engine a `plugins` key and enable `jest`/`vitest`**: upstream's matcher is
framework-aware, knows `it.each` and `it.todo`, and exposes
`assertFunctionNames` so a repo can name its own assertion helpers — which is
precisely the escape hatch the pack rule's 60% comes from lacking. That is a
poly-side change this task could not make; it belongs in `engines/oxc/lint.rs`.

### `placeholder-implementation` — javascript, typescript, tsx — **drop**

Catches a function whose whole body is `throw new Error("Not implemented")`.

**Why oxlint does not cover it.** Nothing in the default plugin set does.
`eslint/no-throw-literal` does not fire (this throws an `Error`), and
`unicorn/prefer-type-error` is about which error class to throw.
`eslint/no-empty` is `restriction`, which poly does not enable — irrelevant
here anyway.

```text

opencode: 0 findings, 1948 js files, 0.0/1000 js files

stagehand: 0 findings, 371 js files, 0.0/1000 js files

copilot-chat: 148 findings, 2173 js files, 68.1/1000 js files
      9  src/extension/mcp/test/vscode-node/util.ts
      9  src/platform/authentication/test/node/copilotToken.spec.ts
      9  src/platform/networking/test/node/networking.spec.ts
      8  src/extension/linkify/test/node/modelFilePathLinkifier.spec.ts
      8  src/extension/test/node/notebookPromptRendering.spec.ts
      8  src/platform/test/node/simulationWorkspaceServices.ts
      7  src/util/common/test/shims/textEditor.ts
      6  src/extension/chatSessions/claude/node/test/claudeCodeModels.spec.ts
      5  src/platform/endpoint/test/node/messagesApi.spec.ts
      5  src/platform/endpoint/test/node/mockEndpoint.ts

vercel-ai: 26 findings, 4051 js files, 6.4/1000 js files
     12  packages/ai/src/model/resolve-model.test.ts
      3  packages/ai/src/translate/stream-translate.test.ts
      3  packages/ai/src/ui/chat.test.ts
      1  examples/ai-e2e-next/app/chat/direct-transport/page.tsx
      1  examples/ai-functions/src/e2e/feature-test-suite.ts
      1  packages/ai/src/test/not-implemented.ts
      1  packages/code-mode/src/tool-invocation.test.ts
      1  packages/langchain/src/transport.ts
      1  packages/open-responses/src/responses/open-responses-language-model.ts
      1  packages/workflow/src/test/test-sandbox.ts

astro: 12 findings, 1374 js files, 8.7/1000 js files
      6  packages/astro/test/units/assets/fonts/core.test.ts
      3  packages/astro/src/core/module-loader/runner.ts
      2  packages/astro/test/units/test-utils.ts
      1  packages/integrations/netlify/src/index.ts

openhands: 0 findings, 836 js files, 0.0/1000 js files

aider: 0 findings, 13 js files, 0.0/1000 js files

pydantic-ai: 0 findings, 1 js files, 0.0/1000 js files

crewai: 0 findings, 3 js files, 0.0/1000 js files
```

**158 of 186 (85%) are in test paths**; 20 of 20 hand-read there are mock
members — a class implementing `IChatEndpoint`, `vscode.FileSystem` or
`Fetcher` and stubbing what the test never calls. The 28-finding non-test
residual measures **15/20 (75%) FP**: `nullImageService`,
`NullWorkspaceMutationManager`, `createLoader(overrides)` factories whose
defaults throw so the caller must override, inline endpoint literals stubbing
an unused `acquireTokenizer`, and deliberate platform guards
(`` `context.next` is not implemented for serverless functions ``).

**Drop.** `throw new Error("Method not implemented.")` is the body TypeScript's
own "implement interface" quick fix writes, and in practice it means "this
member is deliberately unsupported here" — the opposite of what `todo!()` means
in Rust. Path exclusion does not save a rule that is 75% wrong in production
code.

### `swallowed-rejection` — javascript, typescript, tsx — **drop**

Catches an empty `catch` clause and a `.catch(..)` whose handler body is empty.

**Why oxlint does not cover it.** `eslint/no-empty` is `restriction`, and
poly's `DEFAULT_LINT_FILTERS` are `suspicious`, `pedantic`, `complexity` plus
three named rules — verified by running poly over `try { } catch {}`,
`catch (e) {}` and `if (x) {}` and getting back only `no-unused-vars` on the
unused binding. `.catch(() => {})` is outside `no-empty`'s scope regardless.

```text

opencode: 174 findings, 1948 js files, 89.3/1000 js files
     12  packages/opencode/src/cli/cmd/run/runtime.ts
      8  packages/opencode/src/lsp/server.ts
      5  packages/client/src/generated/client.ts
      5  packages/opencode/src/cli/cmd/run/footer.ts
      5  packages/opencode/src/cli/cmd/run/runtime.lifecycle.ts
      5  packages/opencode/src/plugin/snowflake-cortex.ts
      5  packages/opencode/test/cli/tui/plugin-loader.test.ts
      5  packages/tui/src/prompt/stash.tsx
      4  packages/app/src/context/server-session.ts
      4  packages/core/test/util/effect-flock.test.ts

stagehand: 109 findings, 371 js files, 293.8/1000 js files
     18  packages/extension/understudy/locator.ts
     12  packages/extension/understudy/page.ts
      8  packages/extension/understudy/context.ts
      8  packages/extension/understudy/screenshotUtils.ts
      6  packages/extension/handlers/handlerUtils/actHandlerUtils.ts
      6  packages/extension/understudy/clipboard.ts
      4  packages/evals/initStagehand.ts
      4  packages/extension/understudy/a11y/snapshot/coordinateResolver.ts
      4  packages/extension/understudy/a11y/snapshot/focusSelectors.ts
      4  packages/extension/understudy/frameLocator.ts

copilot-chat: 75 findings, 2173 js files, 34.5/1000 js files
      9  src/platform/networking/node/test/chatWebSocketManager.spec.ts
      4  src/extension/chat/vscode-node/chatDebugFileLoggerService.ts
      4  src/extension/chatSessions/copilotcli/vscode-node/test/lockFile.spec.ts
      3  src/extension/chatSessions/copilotcli/common/copilotCLIPrompt.ts
      3  src/extension/intents/node/agentIntent.ts
      3  src/platform/otel/node/test/fileExporters.spec.ts
      3  test/testExecutionInExtension.ts
      2  src/extension/chat/vscode-node/sessionTranscriptService.ts
      2  src/extension/conversation/vscode-node/remoteAgents.ts
      2  src/extension/prompt/vscode-node/gitCommitMessageServiceImpl.ts

vercel-ai: 167 findings, 4051 js files, 41.2/1000 js files
      8  packages/harness-claude-code/src/claude-code-harness.test.ts
      7  packages/harness-acp/src/v1/acp-v1-harness.ts
      6  packages/harness-opencode/src/opencode-harness.ts
      5  examples/ai-functions/src/harness-agent/prepare-sandbox-for-harness.ts
      5  packages/harness/src/utils/bridge-diagnostics.ts
      5  packages/harness-claude-code/src/claude-code-harness.ts
      5  packages/harness-codex/src/codex-harness.ts
      5  packages/harness-pi/src/pi-session.ts
      4  packages/gateway/src/gateway-transcription-model.ts
      4  packages/harness/src/bridge/index.ts

astro: 49 findings, 1374 js files, 35.7/1000 js files
      4  packages/astro/src/cli/preferences/index.ts
      4  packages/astro/src/core/errors/dev/utils.ts
      2  packages/astro/bin/astro.mjs
      2  packages/astro/e2e/test-utils.ts
      2  packages/astro/src/content/mutable-data-store.ts
      2  packages/astro/src/prefetch/index.ts
      2  packages/astro/test/astro-mode.test.ts
      2  packages/integrations/netlify/src/index.ts
      2  packages/integrations/node/src/serve-static.ts
      1  packages/astro/e2e/astro-island-hydration-error.test.ts

openhands: 20 findings, 836 js files, 23.9/1000 js files
      3  electron/main.mjs
      3  scripts/download-node.mjs
      2  electron-builder.config.mjs
      2  scripts/download-uv.mjs
      2  tests/e2e/mock-llm/automations/mock-llm-preset-automation.spec.ts
      2  tests/e2e/mock-llm/skills/mock-llm-skills.spec.ts
      1  __tests__/hooks/query/concurrency-limiter.test.ts
      1  __tests__/hooks/use-download-conversation.test.ts
      1  src/hooks/use-chat-input-profile-state.ts
      1  src/hooks/use-sync-automation-telemetry-consent.ts

aider: 0 findings, 13 js files, 0.0/1000 js files

pydantic-ai: 0 findings, 1 js files, 0.0/1000 js files

crewai: 1 findings, 3 js files, 333.3/1000 js files
      1  lib/crewai/src/crewai/flow/visualization/assets/interactive.js
```

Highest volume of the five (594, 55.2/1000 JS files) and the only one *not*
concentrated in test paths (15%). Hand-read of 20: **19/20 (95%) FP**, in four
deliberate idioms — best-effort cleanup on a `finally`/teardown path
(`session.destroy().catch(() => {})`, `fd.close().catch(() => {})`), optional
parse (`try { JSON.parse(s) } catch {}` with a documented default),
send-on-a-possibly-closed-channel during shutdown, and fire-and-forget where
the empty handler is what *prevents* an unhandled rejection. Exactly one
finding was a real silent discard.

**Drop.** 594 findings at 95% is the shape the pack's own audit rules out, no
path glob helps because the noise is production code, and no syntactic signal
separates best-effort cleanup from a swallowed error. (#23 also asks whether
this belongs in the quality engine instead — on this evidence it belongs
nowhere; the measurement is a property of the idiom, not of which tier
evaluates it.)

### `test-without-assertion` — python — **drop**

Catches a `test_*` function whose body reaches no `assert`, no `raise`, no
`pytest.raises`/`warns`, no `self.assert*` / `assert_*` / `*_assert*` call, no
`snapshot*`, and no `check*`/`verify*`/`validate*`/`ensure*`/`expect*` helper;
exempt when `@pytest.mark.skip`/`xfail`-decorated or when the body calls
`pytest.skip`.

**Why ruff does not cover it.** Verified by running poly over a `test_*`
function whose body is one unasserted call: ruff reports `ANN201` and `F841`
and nothing else. Of the 22 selectors poly enables, `T20`, `TRY`, `BLE`,
`S110`, `C90`, `PLR` and `ARG` are all unrelated. The `PT`
(flake8-pytest-style) group is not enabled and has no assertion-presence rule
in any case.

```text

stagehand: 2 findings, 52 py files, 38.5/1000 py files
      1  packages/sdk-python/tests/test_generated_input_types.py
      1  packages/sdk-python/tests/test_generated_models.py

copilot-chat: 0 findings, 58 py files, 0.0/1000 py files

vercel-ai: 0 findings, 3 py files, 0.0/1000 py files

openhands: 0 findings, 8 py files, 0.0/1000 py files

aider: 7 findings, 122 py files, 57.4/1000 py files
      2  tests/basic/test_commands.py
      2  tests/basic/test_main.py
      1  tests/basic/test_editblock.py
      1  tests/basic/test_sendchat.py
      1  tests/basic/test_wholefile.py

spec-kit: 12 findings, 261 py files, 46.0/1000 py files
      3  tests/integrations/test_integration_catalog.py
      3  tests/test_presets.py
      2  tests/test_extensions.py
      2  tests/test_workflows.py
      1  tests/contract/test_manifest_schema.py
      1  tests/unit/test_bundler_adapters.py

pydantic-ai: 55 findings, 650 py files, 84.6/1000 py files
     11  tests/models/anthropic/test_output.py
      3  tests/realtime/test_session.py
      3  tests/test_agent.py
      3  tests/test_concurrency.py
      3  tests/test_ssrf.py
      2  .github/scripts/test_triage_telemetry.py
      2  tests/durable_exec/test_dbos.py
      2  tests/graph/builder/test_basenode_integration.py
      2  tests/graph/builder/test_graph_execution.py
      2  tests/models/test_openai.py

crewai: 65 findings, 1033 py files, 62.9/1000 py files
     23  lib/crewai-tools/tests/tools/test_nl2sql_security.py
      7  lib/devtools/tests/test_toml_updates.py
      6  lib/cli/tests/test_install_crew.py
      2  lib/cli/tests/skills/test_main.py
      2  lib/crewai/tests/rag/test_error_handling.py
      2  lib/crewai/tests/test_llm.py
      2  lib/crewai/tests/utilities/test_planning_types.py
      1  lib/cli/tests/test_flow_commands.py
      1  lib/cli/tests/test_token_manager.py
      1  lib/crewai/src/crewai/telemetry/telemetry.py
```

Two rounds of narrowing cut 246 → 141. Hand-read of 20: **16/20 (80%) FP**.
Eleven of the sixteen are the Python "call it and let the exception fail the
test" idiom — `tool._validate_query("EXPLAIN SELECT 1")` under a
`# Should not raise` comment, `registry.remove("nonexistent")`,
`test_enforce_parameter_descriptions_noraise`, and one whose own comment reads
"This test doesn't do anything, it's just here to ensure that calls ... don't
cause errors". The rest are helper-encapsulated assertions and `test_*`-named
functions in `scripts/` that pytest never collects.

**Drop.** The rule is *correct* on most of those — the test really does pass
unless something raises — but that is the author's intent, not a defect, and
ast-grep cannot tell them apart. This is `undocumented-unsafe-block` with a
worse ratio.

### `placeholder-implementation` — python — **drop**

Catches `raise NotImplementedError` as an entire function body (a leading
docstring or comment allowed), with carve-outs for `@abstractmethod` /
`@abstractproperty` / `@abstractclassmethod` / `@abstractstaticmethod` /
`@overload` (dotted forms included) and for any method of a class whose bases
name `Protocol` / `ABC` / `ABCMeta` / `Interface`.

**Why ruff does not cover it.** No ruff rule under any selector reports it.
`B027` (flake8-bugbear, and `B` *is* enabled) is the inverse case — an *empty*
method in an ABC that should be abstract — so anything it catches is already
reported. Verified over a module of stub bodies: ruff reports nothing.

```text

stagehand: 0 findings, 52 py files, 0.0/1000 py files

copilot-chat: 1 findings, 58 py files, 17.2/1000 py files
      1  src/extension/completions-core/vscode-node/prompt/src/test/testdata/example.py

vercel-ai: 0 findings, 3 py files, 0.0/1000 py files

openhands: 0 findings, 8 py files, 0.0/1000 py files

aider: 0 findings, 122 py files, 0.0/1000 py files

spec-kit: 0 findings, 261 py files, 0.0/1000 py files

pydantic-ai: 37 findings, 650 py files, 56.9/1000 py files
      6  tests/evals/test_evaluator_base.py
      3  tests/evals/test_reporting.py
      2  tests/durable_exec/temporal/test_model_and_serialization.py
      2  tests/evals/test_utils.py
      2  tests/providers/test_google.py
      2  tests/realtime/test_webrtc.py
      2  tests/test_tools.py
      1  pydantic_ai_slim/pydantic_ai/_output.py
      1  pydantic_ai_slim/pydantic_ai/models/__init__.py
      1  pydantic_ai_slim/pydantic_ai/models/fallback.py

crewai: 9 findings, 1033 py files, 8.7/1000 py files
      3  lib/crewai/tests/test_custom_llm.py
      2  lib/crewai-tools/src/crewai_tools/tools/rag/rag_tool.py
      1  lib/crewai/src/crewai/a2a/auth/server_schemes.py
      1  lib/crewai/src/crewai/rag/embeddings/providers/custom/embedding_callable.py
      1  lib/crewai/tests/test_checkpoint.py
      1  lib/crewai-tools/src/crewai_tools/adapters/mcp_adapter.py
```

33 of 47 (70%) are mocks in test paths. The 14-finding non-test residual was
hand-read in full: **14/14 (100%) FP**. Every one is a deliberate refusal the
carve-outs cannot reach, because the class declares no `ABC`/`Protocol` base
and the method carries no decorator: `'FallbackModel does not have its own
model profile.'`, `'External tools cannot be called directly'`, `'async is not
supported by the CrewAI framework.'`, `'VideoUrl is not supported in OpenAI
Chat Completions user prompts'`, ``'`StepNode` is not meant to be run
directly'``, plus informal abstract bases whose docstring says "Subclasses must
implement". Requiring a message-less raise does not separate them — the four
message-less residual findings are informal abstract bases too.

**Drop.** In Python `raise NotImplementedError` spells "abstract" and
"unsupported on this backend" far more often than "unfinished", and the
`@abstractmethod` / `Protocol` carve-out only catches the formal half.

### `todo-marker` — decisions

**JavaScript/TypeScript: do not add one.** oxlint's
`eslint/no-warning-comments` is **`pedantic`**, which poly enables, and it
fires — verified by linting `// TODO: assert the real shape here` with poly's
defaults and getting `no-warning-comments  Unexpected 'todo' comment`. A pack
rule would be a straight duplicate. (#23 lists `todo-marker` as a surviving
candidate; it does not survive.)

**Python: keep the existing rule `off`.** ruff's `TD` group is not among the 22
selectors, so nothing else reports it, and the rule is exact by construction —
a hand-read of 20 findings across aider, pydantic-ai and crewai found **0/20
false positives**. Volume is the objection, not correctness: 82 findings over
2,130 Python files (38.5/1000), 95.2/1000 in pydantic-ai alone, and the
markers are overwhelmingly deliberate tracked notes (`TODO(v3): remove
`TenacityTransport`, superseded by ...`) rather than forgotten defects. That,
plus the interaction the rule's own note already records — poly's
`[lint.uncomment]` ships `remove_todos = false` precisely so these survive —
keeps it `off`. Promoting it would redden a well-maintained repo for
documenting its own deprecation plan.

---

## Corpus

Ten repositories, pinned SHAs, `--depth 50`, permissive licences (MIT /
Apache-2.0). Six carry JS/TS (10,753 files), six carry Python (2,176 files).
Four have LLM-authored histories by commit trailer — `spec-kit` 42/50 (84%),
`crewai` 25/50 (50%), `openhands` 19/50 (38%), `aider` 22/103 (21%) — and
`copilot-chat` carries `.github/copilot-instructions.md` plus 8/50 Copilot
co-authored commits on the JS/TS side. Seven commit a `CLAUDE.md` or
`AGENTS.md`. `astro` and `vercel-ai` are the mainstream, largely
human-authored controls. Full table with URLs, SHAs and licences in
`scripts/harden/repos.c.tsv`.

Runs used `poly lint --no-workspace --no-cache --config <measure-poly.toml>
--format json`, with `[rules] dirs` pointed at the draft directory, `[rules] builtin
= false`, and `[lint.astgrep] extend_select` naming the three ids (the drafts
ship `off`, so they need opting in). Findings were filtered to
`engine == "astgrep"` and the three rule ids. "Per 1000 files" is per 1000
files *of the rule's own languages*, not per 1000 files scanned.

## Method note

Nothing here was estimated. Every FP rate is a hand-read of the matched line in
full file context, and every "X does not cover this" claim was checked by
running the tool: poly with its shipped defaults over a probe file, plus the
pinned oxc source for rule categories and the default plugin set. Two claims
that seemed safe turned out false on checking — oxlint *does* report `TODO`
comments by default, and oxlint *does* implement `expect-expect` but cannot be
made to run it from poly's config — and both changed a recommendation.
