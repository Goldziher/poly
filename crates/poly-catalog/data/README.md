# Vendored tool catalog

`catalog.json` and `golden.json` are **derived data**, generated from
[mdsf](https://github.com/hougesen/mdsf)'s `tools/*/plugin.json` catalog (MIT,
© 2024 Mads Hougesen — see the repo-root `ATTRIBUTIONS.md`).

- **Source snapshot:** mdsf commit `e926e24b3d08a8304407a7b305e0236db60701f8`.
- **`catalog.json`** — the runtime registry: per tool, its `binary`, `categories`,
  `languages`, `homepage`, and the `commands` map (each command's `arguments`
  vector — with the `$PATH` placeholder — and its `stdin` flag). Deprecated tools
  and deprecated commands are dropped. Embedded into the binary via `include_str!`.
- **`golden.json`** — per-command `(input, output)` fixtures lifted from mdsf's
  command tests, used only by the crate's golden tests (compiled into the test
  binary, never the shipped binary).

Updating the snapshot is a deliberate change: re-run the consolidation against a
newer mdsf checkout, bump the commit above, and re-baseline the golden tests.
The `$PATH` token is the sole argument placeholder; `poly` substitutes it with
the concrete file path at invocation time.

## `overlay.json` — poly's own tools and recipes

Unlike the two files above, `overlay.json` is **hand-written, not derived**. It is
layered over `catalog.json` when the catalog loads: an entry with the name of an
mdsf tool replaces that tool (its `commands` are merged over the mdsf ones, so the
golden fixtures keep naming commands that exist), and any other entry adds a tool
the snapshot lacks (`checkstyle`, `palantir-java-format`, `helm`, `tflint`,
`pyproject-fmt`, ...). Its distinguishing field is `recipe` (see `src/recipe.rs`):
the report format to parse, how files group into invocations, what else decides the
verdict (for the result cache), and pinned, checksummed releases poly can download.

The overlay also corrects mdsf's classification where it is wrong. `pydocstyle`,
`bashate`, `luacheck` and `mh_style` are report-only checkers (their catalog argv
carries no write flag) that mdsf files under `formatter`; the overlay re-declares
them as `linter`, so `poly lint` surfaces their findings instead of the format path
reading a failing exit as "unchanged" (Goldziher/poly#36).
`scripts/gen-catalog-md.py` reads `catalog.json` only, so re-running it reverts
those rows in the website's catalog table; fix them by hand afterwards.
