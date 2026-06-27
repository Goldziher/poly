# Vendored Crate Attributions

## ruff (v0.15.20, commit 03f787e)

**Upstream:** <https://github.com/astral-sh/ruff>
**License:** MIT
**Upstream Cargo.toml:** `<root>/Cargo.toml` (workspace)

The following 9 crates are vendored from the ruff monorepo at commit `03f787e`:

| Crate | Path in upstream |
|---|---|
| `ruff_cache` | `crates/ruff_cache` |
| `ruff_formatter` | `crates/ruff_formatter` |
| `ruff_macros` | `crates/ruff_macros` |
| `ruff_python_ast` | `crates/ruff_python_ast` |
| `ruff_python_formatter` | `crates/ruff_python_formatter` |
| `ruff_python_parser` | `crates/ruff_python_parser` |
| `ruff_python_trivia` | `crates/ruff_python_trivia` |
| `ruff_source_file` | `crates/ruff_source_file` |
| `ruff_text_size` | `crates/ruff_text_size` |

### Modifications

Each crate's `Cargo.toml` has been replaced with a standalone file that uses
explicit version strings instead of `workspace = true` references, and lists
only the dependencies required for the formatting + parsing surface (no Salsa,
no `ruff_db`).

Two source files were modified to remove the optional Salsa/`ruff_db` layer:

- `ruff_python_formatter/src/lib.rs`: removed `use ruff_db::*` imports,
  `pub use crate::db::Db`, `mod db;`, `salsa::Update` from the
  `FormatModuleError` derive, the `impl From<&FormatModuleError> for
  Diagnostic` block, and the `formatted_file()` function (all of which depend
  on `ruff_db` / Salsa).
- `ruff_python_ast/src/name.rs`: removed the
  `#[cfg_attr(feature = "salsa", derive(salsa::Update))]` line on `Name`.
