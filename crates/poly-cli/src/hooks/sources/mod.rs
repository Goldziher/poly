//! Provision catalogs selected by `[[hooks.sources]]` in `poly.toml`, split by
//! the step each concern owns: reading a producer catalog, pinning a source
//! revision in the lock file, materializing a source and choosing an execution
//! path for every selected hook, and merging the result into the native runner
//! model.

mod lock;
mod manifest;
mod merge;
mod provision;
mod select;

#[cfg(test)]
mod test_support;

pub use merge::merge_stage;
pub use provision::provision;
pub use select::ResolvedHook;
