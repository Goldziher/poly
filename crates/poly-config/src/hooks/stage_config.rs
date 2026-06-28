//! [`StageConfig`] — the per-stage `[hooks.<stage>]` table.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::hooks::job::Job;
use crate::hooks::patterns::{Guard, Patterns};

/// Configuration for a single git stage (`[hooks.pre-commit]`, etc.).
///
/// Jobs come from three places, run in this order within the stage:
/// the ordered `jobs` array-of-tables, then the named `commands` map, then the
/// named `scripts` map. For a map entry the key supplies the job `name` when
/// the job omits its own.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct StageConfig {
    /// Run this stage's jobs concurrently.
    pub parallel: bool,
    /// Run jobs sequentially, aborting on the first failure (lefthook "piped").
    pub piped: bool,
    /// Stream job output as it is produced.
    pub follow: bool,
    /// Tags whose jobs are excluded from this stage.
    pub exclude_tags: Vec<String>,
    /// Stage-level file include glob(s).
    pub files: Option<Patterns>,
    /// Stage-level file exclude glob(s).
    pub exclude: Option<Patterns>,
    /// Skip guard — when active, the whole stage is skipped.
    pub skip: Option<Guard>,
    /// Only guard — the stage runs *only* when active.
    pub only: Option<Guard>,
    /// Fail the stage if its jobs dirty the working tree.
    pub fail_on_changes: bool,
    /// Guard command: exit 0 runs the stage, non-zero (or missing tool) **skips
    /// it with a warning** rather than aborting.
    pub precondition: Option<String>,
    /// Command(s) run after `precondition` passes and before the jobs;
    /// failure aborts the stage. Run sequentially.
    pub before: Option<Patterns>,
    /// Command(s) run after the stage's jobs succeed; non-zero aborts.
    pub after: Option<Patterns>,
    /// Ordered jobs (`[[hooks.<stage>.jobs]]`).
    pub jobs: Vec<Job>,
    /// Named command jobs (`[hooks.<stage>.commands.<name>]`).
    pub commands: BTreeMap<String, Job>,
    /// Named script jobs (`[hooks.<stage>.scripts.<name>]`).
    pub scripts: BTreeMap<String, Job>,
}

impl StageConfig {
    /// Iterate over every job in this stage paired with a stable label for
    /// diagnostics: the explicit `name`, else the map key, else `jobs[<index>]`.
    ///
    /// Order is `jobs` (array), then `commands`, then `scripts`.
    pub fn labeled_jobs(&self) -> impl Iterator<Item = (String, &Job)> {
        let ordered = self.jobs.iter().enumerate().map(|(index, job)| {
            let label = job.name.clone().unwrap_or_else(|| format!("jobs[{index}]"));
            (label, job)
        });
        let commands = self
            .commands
            .iter()
            .map(|(key, job)| (job.name.clone().unwrap_or_else(|| key.clone()), job));
        let scripts = self
            .scripts
            .iter()
            .map(|(key, job)| (job.name.clone().unwrap_or_else(|| key.clone()), job));
        ordered.chain(commands).chain(scripts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_jobs_commands_and_scripts() {
        let stage: StageConfig = toml::from_str(
            r#"
parallel = true
precondition = "command -v cargo"
before = "echo before"
after = ["echo a", "echo b"]

[[jobs]]
run = "echo one"

[commands.fmt]
run = "cargo fmt"

[scripts.lint]
script = "lint.sh"
runner = "bash"
"#,
        )
        .unwrap();
        assert!(stage.parallel);
        assert_eq!(stage.precondition.as_deref(), Some("command -v cargo"));
        assert_eq!(stage.before.as_ref().unwrap().len(), 1);
        assert_eq!(stage.after.as_ref().unwrap().len(), 2);
        assert_eq!(stage.jobs.len(), 1);
        assert!(stage.commands.contains_key("fmt"));
        assert!(stage.scripts.contains_key("lint"));

        let labels: Vec<String> = stage.labeled_jobs().map(|(label, _)| label).collect();
        assert_eq!(labels, vec!["jobs[0]", "fmt", "lint"]);
    }

    #[test]
    fn unknown_stage_field_is_rejected() {
        let result: Result<StageConfig, _> = toml::from_str("bogus = true");
        assert!(result.is_err(), "deny_unknown_fields must reject `bogus`");
    }
}
