//! A staged snapshot holds tracked files only, so a job that needs the
//! untracked `node_modules` fails under the commit gate while passing in the
//! worktree. This finds that configuration before the first commit does.

use std::path::Path;

use poly_config::{HooksConfig, Job, Stage};

/// Commands that resolve their own binaries or modules from `node_modules`.
const NODE_TOOLS: &[&str] = &[
    "npx",
    "pnpm",
    "yarn",
    "npm",
    "bunx",
    "node",
    "prettier",
    "eslint",
    "tsc",
    "vitest",
    "jest",
    "biome",
    "oxlint",
    "stylelint",
    "markdownlint",
    "markdownlint-cli2",
    "cspell",
];

/// Stages whose jobs run against the staged snapshot.
const ISOLATED_STAGES: &[Stage] = &[Stage::PreCommit, Stage::PreMergeCommit];

fn job_text(job: &Job) -> impl Iterator<Item = &str> {
    job.run
        .iter()
        .chain(job.script.iter())
        .chain(job.runner.iter())
        .chain(job.args.iter())
        .map(String::as_str)
}

fn needs_node_modules(job: &Job) -> bool {
    job_text(job).any(|text| {
        text.contains("node_modules")
            || text
                .split(|c: char| c.is_whitespace() || matches!(c, ';' | '&' | '|' | '(' | ')'))
                .any(|word| NODE_TOOLS.contains(&word.rsplit('/').next().unwrap_or(word)))
    })
}

/// The jobs that need `node_modules` while isolation is on, or empty when
/// nothing is wrong: isolation off, `node_modules` already listed in
/// `snapshot_include`, or no `node_modules` in `root` to miss.
#[must_use]
pub fn jobs_missing_node_modules(hooks: &HooksConfig, root: &Path) -> Vec<String> {
    let listed = hooks
        .snapshot_include
        .as_slice()
        .iter()
        .any(|entry| entry.trim_end_matches('/') == "node_modules");
    if hooks.isolate == Some(false) || listed || !root.join("node_modules").is_dir() {
        return Vec::new();
    }
    hooks
        .stage_configs
        .iter()
        .filter(|(stage, _)| ISOLATED_STAGES.contains(stage))
        .flat_map(|(_, config)| config.labeled_jobs())
        .filter(|(_, job)| needs_node_modules(job))
        .map(|(label, _)| label)
        .collect()
}

/// The one-line warning for `jobs`, with both remedies.
#[must_use]
pub fn warning(jobs: &[String]) -> String {
    format!(
        "the commit gate runs [hooks] jobs in a staged snapshot without untracked files, but {} use node_modules; \
         expect \"command not found\" or missing modules there. Add `snapshot_include = [\"node_modules\"]` under \
         [hooks] to link it in, or set `isolate = false`",
        jobs.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hooks(toml: &str) -> HooksConfig {
        toml::from_str(toml).expect("parse [hooks]")
    }

    const PRETTIER: &str = "[[pre-commit.jobs]]\nname = \"fmt-js\"\nrun = \"npx prettier --check\"\n";

    #[test]
    fn flags_a_node_job_when_node_modules_exists_and_isolation_is_on() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("node_modules")).unwrap();
        assert_eq!(jobs_missing_node_modules(&hooks(PRETTIER), dir.path()), vec!["fmt-js"]);
    }

    #[test]
    fn silent_without_node_modules_in_the_repo() {
        let dir = tempfile::tempdir().unwrap();
        assert!(jobs_missing_node_modules(&hooks(PRETTIER), dir.path()).is_empty());
    }

    #[test]
    fn silent_once_isolation_is_off_or_node_modules_is_included() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("node_modules")).unwrap();
        for prefix in ["isolate = false\n", "snapshot_include = [\"node_modules\"]\n"] {
            let config = hooks(&format!("{prefix}{PRETTIER}"));
            assert!(jobs_missing_node_modules(&config, dir.path()).is_empty(), "{prefix}");
        }
    }

    #[test]
    fn ignores_jobs_that_do_not_touch_node() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("node_modules")).unwrap();
        let config = hooks("[[pre-commit.jobs]]\nname = \"py\"\nrun = \"ruff check\"\n");
        assert!(jobs_missing_node_modules(&config, dir.path()).is_empty());
    }
}
