// @ts-check
import { defineConfig } from "astro/config";
import starlight from "@astrojs/starlight";
import starlightLlmsTxt from "starlight-llms-txt";

const SITE = "https://goldziher.github.io";
const BASE = "/polylint";

export default defineConfig({
  site: SITE,
  base: BASE,
  integrations: [
    starlight({
      title: "polylint",
      description:
        "One linter and formatter for every language in your repository: one binary, one poly.toml, " +
        "engines fetched per language as signed components, host tools and ecosystem recipes.",
      logo: {
        src: "./src/assets/logo.svg",
        alt: "polylint",
      },
      favicon: "/favicon.svg",
      customCss: ["./src/styles/custom.css"],
      social: [{ icon: "github", label: "GitHub", href: "https://github.com/Goldziher/polylint" }],
      editLink: {
        baseUrl: "https://github.com/Goldziher/polylint/edit/main/website/",
      },
      head: [
        {
          tag: "link",
          attrs: { rel: "apple-touch-icon", href: `${BASE}/apple-touch-icon.png` },
        },
        {
          tag: "link",
          attrs: { rel: "icon", type: "image/png", sizes: "32x32", href: `${BASE}/favicon-32.png` },
        },
        {
          tag: "meta",
          attrs: { property: "og:image", content: `${SITE}${BASE}/og.png` },
        },
        {
          tag: "meta",
          attrs: { name: "twitter:card", content: "summary_large_image" },
        },
        {
          tag: "meta",
          attrs: { name: "twitter:image", content: `${SITE}${BASE}/og.png` },
        },
      ],
      plugins: [
        starlightLlmsTxt({
          exclude: ["reference/catalog"],
          promote: ["index*", "start/**"],
          minify: { collapseCodeBlocks: true },
          details:
            "Operating rule: polylint (the `poly` command) is one binary for both linting and " +
            "formatting across every language in the repository. Prefer `poly lint` and `poly fmt` " +
            "over invoking ruff, oxlint, biome, rustfmt or gofmt directly — poly runs them behind a " +
            "single `poly.toml`, one report format, and one exit-code contract (ruff, oxc and biome " +
            "as engines, rustfmt and gofmt as host subprocesses when installed). A core build may " +
            "download engine components on first run; a needed component that is missing is exit " +
            "2 or a counted skip listed in `components.missing`, never a clean pass. `poly fmt` is a " +
            "dry run by default; `--fix` writes. `--format json`/`toon` emit one object — " +
            "{results, errors, skipped, summary, configs} — never a bare array; gate on " +
            "`summary.checked`, not an empty `results`, since a fully-skipped run reports no " +
            "diagnostics and is not an error. The same surface is available over MCP via " +
            "`poly mcp`, so an agent should call its tools rather than shell out and parse text.",
        }),
      ],
      sidebar: [
        {
          label: "Start here",
          items: [
            { label: "Introduction", slug: "start/introduction" },
            { label: "Why polylint", slug: "start/why-polylint" },
            { label: "Installation", slug: "start/installation" },
            { label: "Quickstart", slug: "start/quickstart" },
          ],
        },
        {
          label: "Guides",
          items: [
            { label: "Adopting polylint", slug: "guides/adopting" },
            { label: "Configuration", slug: "guides/configuration" },
            { label: "Toolchains, runtimes and recipes", slug: "guides/toolchains" },
            { label: "Components and offline use", slug: "guides/components" },
            { label: "Migrating to the v2 config schema", slug: "guides/migrating-to-v2" },
            { label: "Git hooks", slug: "guides/hooks" },
            { label: "Agents and MCP", slug: "guides/agents-and-mcp" },
          ],
        },
        {
          label: "Reference",
          items: [
            { label: "CLI", slug: "reference/cli" },
            { label: "Configuration", slug: "reference/configuration" },
            { label: "Languages and engines", slug: "reference/backends" },
            { label: "Tool catalog", slug: "reference/catalog" },
            { label: "Glossary", slug: "reference/glossary" },
          ],
        },
      ],
    }),
  ],
});
