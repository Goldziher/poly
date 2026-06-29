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
