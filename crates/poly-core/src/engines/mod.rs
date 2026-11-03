//! Backend implementations. Each backend is a self-contained module implementing
//! [`crate::engine::Engine`]. New backends are added here and wired into the
//! crate-private registry that routes each language to its backend(s). The python, php, sql, nix, typos,
//! astgrep, uncomment and misc (dockerfile, dotenv, hcl, graphql) families live in their own
//! `poly-engines-*` crates and are re-exported here at their old paths.

#[cfg(feature = "engine-web")]
pub mod adrlint;
#[cfg(feature = "engine-astgrep")]
pub use poly_engines_astgrep as astgrep;
#[cfg(feature = "engine-biome")]
pub mod biome_common;
#[cfg(feature = "engine-biome")]
pub mod biome_css;
#[cfg(feature = "engine-biome")]
pub mod biome_graphql;
pub mod catalog_tool;
pub mod config_keys;
#[cfg(feature = "engine-web")]
mod core_options;
pub mod css_hacks;
#[cfg(feature = "engine-dockerfile")]
pub use poly_engines_misc::dockerfile;
#[cfg(feature = "engine-dotenv")]
pub use poly_engines_misc::dotenv;
pub mod format_scope;
pub(crate) use poly_grammar as grammar;

/// Whether the tree-sitter grammar `name` can be loaded right now. Offline this
/// never downloads: a grammar that is not cached reports `false`.
#[must_use]
pub fn grammar_available(name: &str) -> bool {
    grammar::language(name).is_some()
}
#[cfg(feature = "engine-graphql")]
pub use poly_engines_misc::graphql;
#[cfg(feature = "engine-hcl")]
pub use poly_engines_misc::hcl;
#[cfg(feature = "engine-ini")]
pub mod ini;
pub mod line_index;
#[cfg(feature = "engine-php")]
pub use poly_engines_php::mago;
#[cfg(feature = "engine-web")]
pub mod malva;
#[cfg(feature = "engine-web")]
mod markdown_embed;
#[cfg(feature = "engine-web")]
mod markdown_fmt;
#[cfg(feature = "engine-web")]
pub mod markup_fmt;
pub mod native_tool;
#[cfg(feature = "engine-nix")]
pub use poly_engines_nix::nixfmt;
#[cfg(feature = "engine-web")]
pub mod oxc;
#[cfg(feature = "engine-quality")]
pub mod quality;
#[cfg(feature = "engine-ruby")]
pub mod rubyfmt;
#[cfg(feature = "engine-python")]
pub use poly_engines_python::ruff;
pub mod rule_config;
#[cfg(feature = "engine-web")]
pub mod rumdl;
#[cfg(feature = "engine-sql")]
pub use poly_engines_sql::sqruff;
#[cfg(feature = "engine-web")]
pub mod taplo;
pub mod template;
pub mod treesitter;
#[cfg(feature = "engine-typos")]
pub use poly_engines_typos::typos;
#[cfg(feature = "engine-uncomment")]
pub use poly_engines_uncomment::uncomment;
#[cfg(feature = "engine-web")]
pub mod yaml;
