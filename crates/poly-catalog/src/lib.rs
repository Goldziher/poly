//! `poly-catalog` — the vendored tool-catalog registry.
//!
//! poly wraps best-in-class Rust crates in-process as tier-1 backends, falls
//! through to a tree-sitter generic tier, and — for languages whose canonical
//! tool is a standalone CLI with no usable Rust library — shells out to that CLI
//! (ADR 0013/0014). This crate holds the data that drives that last tier: a
//! registry of `tool → binary → argv (with the `$PATH` placeholder) / stdin →
//! languages / categories`, derived from [mdsf](https://github.com/hougesen/mdsf)
//! (MIT). See `data/README.md` for provenance and the snapshot commit.
//!
//! The registry is parsed once from the embedded `data/catalog.json` and cached
//! for the process lifetime via [`Catalog::get`].
//!
//! ```
//! let catalog = poly_catalog::Catalog::get();
//! let shfmt = catalog.tool("shfmt").expect("shfmt is in the catalog");
//! assert_eq!(shfmt.binary, "shfmt");
//! assert!(shfmt.is_formatter());
//! ```

mod model;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::OnceLock;

use anyhow::{Context, Result};

pub use model::{CATEGORY_FORMATTER, CATEGORY_LINTER, CATEGORY_SPELL_CHECK, Command, PATH_PLACEHOLDER, Tool};

/// The vendored catalog JSON, embedded at compile time.
const CATALOG_JSON: &str = include_str!("../data/catalog.json");

/// The parsed tool registry: every [`Tool`] plus a name → index for O(1) lookup.
///
/// Obtain the shared instance with [`Catalog::get`]; it is parsed at most once
/// per process.
#[derive(Debug)]
pub struct Catalog {
    tools: Vec<Tool>,
    index: HashMap<String, usize>,
}

impl Catalog {
    /// The process-wide catalog, parsed from the embedded JSON on first use.
    ///
    /// # Panics
    ///
    /// Panics only if the **vendored** `data/catalog.json` is malformed — a build
    /// integrity error, not a runtime input error (the data ships in the binary).
    pub fn get() -> &'static Catalog {
        static CATALOG: OnceLock<Catalog> = OnceLock::new();
        CATALOG.get_or_init(|| Catalog::parse(CATALOG_JSON).expect("vendored data/catalog.json must be valid"))
    }

    /// Parse a catalog from a JSON array of [`Tool`]s, building the name index.
    fn parse(json: &str) -> Result<Catalog> {
        let tools: Vec<Tool> = serde_json::from_str(json).context("parsing vendored tool catalog")?;
        let index = tools
            .iter()
            .enumerate()
            .map(|(position, tool)| (tool.name.clone(), position))
            .collect();
        Ok(Catalog { tools, index })
    }

    /// Look up a tool by its catalog name (e.g. `"shfmt"`).
    pub fn tool(&self, name: &str) -> Option<&Tool> {
        self.index.get(name).map(|&position| &self.tools[position])
    }

    /// Every tool in the catalog, in load order.
    pub fn tools(&self) -> &[Tool] {
        &self.tools
    }

    /// Tools that advertise the given mdsf `language` identifier.
    pub fn tools_for_language<'a>(&'a self, language: &'a str) -> impl Iterator<Item = &'a Tool> + 'a {
        self.tools
            .iter()
            .filter(move |tool| tool.languages.iter().any(|candidate| candidate == language))
    }
}
