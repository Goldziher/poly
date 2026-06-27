//! `[hooks]` — configuration for `poly hooks` (the polyhooks git-hook runner).
//!
//! Two kinds of hook live here:
//! - **Builtin** (`[hooks.builtin]`): poly's own tools (`polylint`, `polyfmt`,
//!   `commit`) run **in-process** — no repo clone, no subprocess. Each is either
//!   a bare `true` (enable with default stages) or a table for per-hook stages.
//! - **Foreign** (`[[hooks.repo]]`): pre-commit-compatible hook repositories,
//!   cloned and run the way `pre-commit`/`prek` do, described by the upstream
//!   `.pre-commit-hooks.yaml` manifest.

use serde::{Deserialize, Deserializer};

/// `[hooks]` table.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct HooksConfig {
    /// Default stages applied to hooks that do not specify their own.
    pub stages: Vec<String>,
    /// poly's own in-process tools.
    pub builtin: BuiltinHooks,
    /// Foreign cloned hook repositories (`[[hooks.repo]]`).
    #[serde(rename = "repo")]
    pub repos: Vec<RepoHooks>,
}

/// `[hooks.builtin]` — poly's first-class in-process hooks.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct BuiltinHooks {
    /// The `polylint` linter hook.
    pub polylint: BuiltinHook,
    /// The `polyfmt` formatter hook.
    pub polyfmt: BuiltinHook,
    /// The `poly commit` message-lint hook.
    pub commit: BuiltinHook,
}

/// One builtin hook. Accepts either a bare boolean (`polylint = true`) or a
/// table (`polyfmt = { stages = ["pre-commit"] }`); a table without an explicit
/// `enabled` key is treated as enabled.
#[derive(Debug, Clone, Default)]
pub struct BuiltinHook {
    /// Whether this builtin hook is active.
    pub enabled: bool,
    /// Stages this hook runs in; empty means inherit [`HooksConfig::stages`].
    pub stages: Vec<String>,
}

/// On-disk form of a builtin hook: bare toggle or a table.
#[derive(Deserialize)]
#[serde(untagged)]
enum BuiltinHookRepr {
    Toggle(bool),
    Table(BuiltinHookTable),
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct BuiltinHookTable {
    enabled: Option<bool>,
    stages: Vec<String>,
}

impl<'de> Deserialize<'de> for BuiltinHook {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match BuiltinHookRepr::deserialize(deserializer)? {
            BuiltinHookRepr::Toggle(enabled) => Ok(BuiltinHook {
                enabled,
                stages: Vec::new(),
            }),
            // Presence of a table implies the hook is enabled unless it says otherwise.
            BuiltinHookRepr::Table(table) => Ok(BuiltinHook {
                enabled: table.enabled.unwrap_or(true),
                stages: table.stages,
            }),
        }
    }
}

/// A foreign hook repository (`[[hooks.repo]]`).
#[derive(Debug, Clone, Deserialize)]
pub struct RepoHooks {
    /// Repository URL (or a local path / sentinel recognized by the runner).
    pub repo: String,
    /// Revision (tag/sha) to clone; `None` for local repos.
    #[serde(default)]
    pub rev: Option<String>,
    /// Hooks selected from this repository's `.pre-commit-hooks.yaml` manifest.
    #[serde(default)]
    pub hooks: Vec<RepoHook>,
}

/// One hook selected from a foreign repository.
#[derive(Debug, Clone, Deserialize)]
pub struct RepoHook {
    /// Hook id as declared in the repository's `.pre-commit-hooks.yaml`.
    pub id: String,
    /// Extra arguments appended to the hook invocation.
    #[serde(default)]
    pub args: Vec<String>,
    /// Stages this hook runs in; empty means inherit [`HooksConfig::stages`].
    #[serde(default)]
    pub stages: Vec<String>,
    /// Regex of file paths to exclude from this hook.
    #[serde(default)]
    pub exclude: Option<String>,
}
