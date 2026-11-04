//! Pre-plan resolution of downloadable engine components.
//!
//! Before a run plans, [`resolve`] derives which components it needs (the discovered
//! `(config, language)` pairs, explicit `[engines.X]` tables and `[components] enable`), makes
//! each usable or records why not, and hands the planner ready component engines, real or
//! stand-ins for an unusable component. It is the only place a component is ever installed:
//! the host it builds refuses to load anything it did not report usable, so the per-file
//! workers can load but never reach the network.
//!
//! A component replaces an engine only for families the binary does not link. The default
//! (`all-engines`) build links every family, so [`resolve`] finds nothing to do there and never
//! builds a host.

#[cfg(not(feature = "dev-lock"))]
mod embedded;
mod needed;
mod preplan;
mod report;
mod seam;
mod select;

pub(crate) use preplan::resolve;
pub use report::{ComponentsReport, MissingComponent};
pub use seam::ComponentInjection;
#[cfg(feature = "dev-lock")]
pub use seam::LOCK_ENV;
pub(crate) use seam::run_index;
pub use select::{component_is_linked, default_cache_root, host_options, needed_component_names};

pub use needed::register_linked_features;
pub(crate) use needed::{feature_is_linked, needed_components, selected_components};

#[cfg(test)]
mod tests;
