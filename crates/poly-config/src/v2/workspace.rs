//! `[workspace]` — the whole-project phase of `poly lint`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// When `poly lint` runs the whole-project phase (`cargo clippy`, `cargo-deny`,
/// project-level type checkers, `workspace = true` hook jobs).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunMode {
    /// Run it for repository-wide invocations (`poly lint .`), skip it when
    /// explicit paths are named.
    #[default]
    Auto,
    /// Always run it.
    Always,
    /// Never run it.
    Never,
}

/// `[workspace]` — replaces v1's `[lint] workspace` toggle. The v1 `[workspace]
/// root` marker is the top-level `root` key in v2.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct WorkspaceV2 {
    /// When the whole-project phase runs. Defaults to `"auto"`.
    pub run: RunMode,
}
