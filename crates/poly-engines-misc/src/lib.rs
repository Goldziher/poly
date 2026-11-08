//! The misc engine family: Dockerfile, dotenv, HCL, INI and GraphQL (pretty_graphql formatter),
//! each behind a feature of the same name.

#[cfg(feature = "dockerfile")]
pub mod dockerfile;
#[cfg(feature = "dotenv")]
pub mod dotenv;
#[cfg(feature = "graphql")]
pub mod graphql;
#[cfg(feature = "graphql")]
mod graphql_mixed;
#[cfg(feature = "hcl")]
pub mod hcl;
#[cfg(feature = "ini")]
pub mod ini;

use poly_engine_api::engine::Engine;
use poly_engine_api::language::Language;

/// The engines this family registers for `language`; empty when the family (or the feature
/// serving that language) is not compiled in.
pub fn engines_for(language: &Language) -> Vec<Box<dyn Engine>> {
    match language {
        #[cfg(feature = "dockerfile")]
        Language::Dockerfile => vec![Box::new(dockerfile::DockerfileEngine)],
        #[cfg(feature = "dotenv")]
        Language::Dotenv => vec![Box::new(dotenv::DotenvEngine)],
        #[cfg(feature = "graphql")]
        Language::GraphQl => vec![Box::new(graphql::GraphQlEngine)],
        #[cfg(feature = "hcl")]
        Language::Hcl => vec![Box::new(hcl::HclEngine)],
        #[cfg(feature = "ini")]
        Language::Ini => vec![Box::new(ini::IniEngine)],
        _ => Vec::new(),
    }
}
