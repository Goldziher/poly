//! Backend implementations. Each backend is a self-contained module implementing
//! [`crate::engine::Engine`]. New backends are added here and wired into the
//! crate-private registry that routes each language to its backend(s).

#[cfg(feature = "engine-web")]
pub mod adrlint;
#[cfg(feature = "engine-astgrep")]
pub mod astgrep;
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
pub mod dockerfile;
#[cfg(feature = "engine-dotenv")]
pub mod dotenv;
pub mod format_scope;
pub(crate) mod grammar;
#[cfg(feature = "engine-graphql")]
pub mod graphql;
#[cfg(feature = "engine-hcl")]
pub mod hcl;
#[cfg(feature = "engine-ini")]
pub mod ini;
pub mod line_index;
#[cfg(feature = "engine-php")]
pub mod mago;
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
pub mod nixfmt;
#[cfg(feature = "engine-web")]
pub mod oxc;
#[cfg(feature = "engine-quality")]
pub mod quality;
#[cfg(feature = "engine-ruby")]
pub mod rubyfmt;
#[cfg(feature = "engine-python")]
pub mod ruff;
pub mod rule_config;
#[cfg(feature = "engine-web")]
pub mod rumdl;
#[cfg(feature = "engine-sql")]
pub mod sqruff;
#[cfg(feature = "engine-web")]
pub mod taplo;
pub mod template;
pub mod treesitter;
#[cfg(feature = "engine-typos")]
pub mod typos;
#[cfg(feature = "engine-uncomment")]
pub mod uncomment;
#[cfg(feature = "engine-web")]
pub mod yaml;
