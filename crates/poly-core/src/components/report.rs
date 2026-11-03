//! What a run reports about the components it needed.

use serde::Serialize;

/// A needed component that could not be used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct MissingComponent {
    /// The component's name.
    pub name: String,
    /// Why: `offline`, `install_never`, `download_failed`, `verify_failed` or `incompatible`.
    pub kind: String,
    /// The reason files it would have handled are skipped or failed with.
    pub reason: String,
    /// Whether this fails the run (exit 2): `on_missing = "fail"`, or an install that was
    /// attempted and failed. `false` only for a component `on_missing = "skip"` turned into skips.
    pub blocking: bool,
}

/// The `components` block of a lint or format document.
///
/// All three lists are empty for a run that needed no downloadable component, which is every
/// run of the default (bundled) build, and the block is then left out of the document.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct ComponentsReport {
    /// Components the run could use, installed before or by this run.
    pub used: Vec<String>,
    /// The subset of `used` that this run downloaded and verified.
    pub installed_now: Vec<String>,
    /// Components the run needed and could not use.
    pub missing: Vec<MissingComponent>,
}

impl ComponentsReport {
    /// Whether the run needed no component at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.used.is_empty() && self.installed_now.is_empty() && self.missing.is_empty()
    }

    /// Whether a missing component fails the run.
    #[must_use]
    pub fn blocking(&self) -> bool {
        self.missing.iter().any(|m| m.blocking)
    }
}
