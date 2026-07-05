//! The [`Job`] and [`JobCache`] types — one runnable unit within a stage.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::HookCacheMode;
use crate::hooks::patterns::{Guard, Patterns};

/// One runnable unit within a stage (lefthook "command" or "script").
///
/// A job runs exactly one of `run` (a shell command) **xor** `script` (a script
/// file interpreted by `runner`); [`super::HooksConfig::validate`] enforces this.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Job {
    /// Display name; defaults to the map key when defined under
    /// `[hooks.<stage>.commands.<name>]` / `.scripts.<name>`.
    pub name: Option<String>,
    /// Shell command to run. Mutually exclusive with `script`.
    pub run: Option<String>,
    /// Script file to run. Mutually exclusive with `run`; requires `runner`.
    pub script: Option<String>,
    /// Interpreter used to execute `script` (e.g. `bash`, `python`).
    pub runner: Option<String>,
    /// Extra arguments appended to the invocation.
    pub args: Vec<String>,
    /// Glob(s) selecting which changed files this job receives.
    pub glob: Option<Patterns>,
    /// File include glob(s) (alias-style scoping alongside `glob`).
    pub files: Option<Patterns>,
    /// File exclude glob(s).
    pub exclude: Option<Patterns>,
    /// File-type filters (e.g. `text`, `executable`).
    pub file_types: Vec<String>,
    /// Run the job from this subdirectory.
    pub root: Option<String>,
    /// Skip guard — when active, the job does not run.
    pub skip: Option<Guard>,
    /// Only guard — the job runs *only* when active.
    pub only: Option<Guard>,
    /// Tags for selective inclusion/exclusion.
    pub tags: Vec<String>,
    /// Per-job environment variables (merged over the global `[hooks].env`).
    pub env: BTreeMap<String, String>,
    /// Message printed when the job fails.
    pub fail_text: Option<String>,
    /// Lower values run first within a stage (default `0`).
    pub priority: i64,
    /// When the job modifies files and exits 0, the runner `git add`s the
    /// matched files and continues; only a non-zero exit fails the stage.
    pub stage_fixed: bool,
    /// Whole-workspace job: it compiles or analyses the entire project (e.g.
    /// `cargo clippy`, a type checker like `pyrefly`) rather than a per-file
    /// set. When staged isolation is active such a job runs against a
    /// non-destructive snapshot of the staged index, so it never sees unstaged
    /// worktree edits or untracked files. Default `false` (per-file).
    pub workspace: bool,
    /// The job needs an interactive terminal.
    pub interactive: bool,
    /// Feed matched file contents to the job on stdin.
    pub use_stdin: bool,
    /// Per-job result-cache declaration.
    pub cache: Option<JobCache>,
}

/// Per-job result-cache declaration.
///
/// `mode` reuses the crate-wide [`HookCacheMode`]; `inputs` lists glob sets the
/// command depends on; `compiler` opts the job into tier-2 sccache env
/// injection.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct JobCache {
    /// Override the cache mode for this job; `None` inherits `[cache.results]`.
    pub mode: Option<HookCacheMode>,
    /// Glob sets the command's output depends on (e.g.
    /// `["**/*.rs", "Cargo.toml", "rust-toolchain.toml"]`).
    pub inputs: Vec<Patterns>,
    /// Opt into tier-2 sccache env injection (`RUSTC_WRAPPER`, …).
    pub compiler: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_run_job() {
        let job: Job = toml::from_str(
            r#"
run = "cargo fmt --check"
priority = -1
tags = ["rust"]
"#,
        )
        .unwrap();
        assert_eq!(job.run.as_deref(), Some("cargo fmt --check"));
        assert_eq!(job.priority, -1);
        assert_eq!(job.tags, vec!["rust".to_string()]);
        assert!(job.script.is_none());
    }

    #[test]
    fn parses_job_cache_with_string_or_array_inputs() {
        let job: Job = toml::from_str(
            r#"
run = "cargo clippy"
[cache]
mode = "aggressive"
compiler = true
inputs = ["**/*.rs", "Cargo.toml"]
"#,
        )
        .unwrap();
        let cache = job.cache.expect("cache present");
        assert_eq!(cache.mode, Some(HookCacheMode::Aggressive));
        assert!(cache.compiler);
        assert_eq!(cache.inputs.len(), 2);
        assert_eq!(cache.inputs[0].as_slice(), &["**/*.rs".to_string()]);
    }

    #[test]
    fn unknown_job_field_is_rejected() {
        let result: Result<Job, _> = toml::from_str(
            r#"run = "x"
bogus = true"#,
        );
        assert!(result.is_err(), "deny_unknown_fields must reject `bogus`");
    }
}
