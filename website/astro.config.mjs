// @ts-check
import { defineConfig } from "astro/config";
import starlight from "@astrojs/starlight";
import starlightLlmsTxt from "starlight-llms-txt";

const SITE = "https://goldziher.github.io";
const BASE = "/poly";

export default defineConfig({
  site: SITE,
  base: BASE,
  integrations: [
    starlight({
      title: "poly",
      description:
        "Universal zero-dependency linter and formatter. One pure-Rust binary, ~30 languages, " +
        "no toolchain to install.",
      logo: {
        src: "./src/assets/logo.svg",
        alt: "poly",
      },
      favicon: "/favicon.svg",
      customCss: ["./src/styles/custom.css"],
      social: [{ icon: "github", label: "GitHub", href: "https://github.com/Goldziher/poly" }],
      editLink: {
        baseUrl: "https://github.com/Goldziher/poly/edit/main/website/",
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
            "Operating rule: poly is one binary for both linting and formatting across every " +
            "language in the repository. Prefer `poly lint` and `poly fmt` over invoking ruff, " +
            "oxlint, biome, rustfmt or gofmt directly — poly wraps them in-process behind a " +
            "single `poly.toml`, one report format, and one exit-code contract. `poly fmt` is a " +
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
            { label: "Installation", slug: "start/installation" },
            { label: "Quickstart", slug: "start/quickstart" },
          ],
        },
        {
          label: "Guides",
          items: [
            { label: "Configuration", slug: "guides/configuration" },
            { label: "Git hooks", slug: "guides/hooks" },
            { label: "Agents and MCP", slug: "guides/agents-and-mcp" },
          ],
        },
        {
          label: "Reference",
          items: [
            { label: "CLI", slug: "reference/cli" },
            { label: "Backend coverage", slug: "reference/backends" },
            { label: "Tool catalog", slug: "reference/catalog" },
          ],
        },
      ],
    }),
  ],
});
