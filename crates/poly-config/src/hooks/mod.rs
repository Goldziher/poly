//! `[hooks]` — configuration for `poly hooks` (the polyhooks git-hook runner).
//!
//! The schema is **lefthook-style and inline**: hooks are declared directly in
//! `poly.toml`, keyed by git stage (`[hooks.pre-commit]`, `[hooks.pre-push]`,
//! …). Imported pre-commit repositories are no longer supported — every hook is
//! either a poly **builtin** (`[hooks.builtin]`, run in-process) or an inline
//! [`Job`] under a stage.
//!
//! The `[hooks]` table partitions its keys into three reserved keys —
//! [`stages`](HooksConfig::stages), [`env`](HooksConfig::env), and
//! [`builtin`](HooksConfig::builtin) — and per-stage keys, each of which must
//! name a valid git [`Stage`] and whose value is a [`StageConfig`]. An unknown
//! key that is neither reserved nor a valid stage is a hard error.

mod builtin;
mod job;
mod patterns;
mod stage;
mod stage_config;

use std::collections::BTreeMap;
use std::fmt;

use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};

pub use builtin::{BuiltinHook, BuiltinHooks};
pub use job::{Job, JobCache};
pub use patterns::{Guard, GuardCondition, GuardMatch, Patterns};
pub use stage::{ParseStageError, Stage};
pub use stage_config::StageConfig;

/// `[hooks]` table — the inline, lefthook-style git-hook configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HooksConfig {
    /// Default stages applied to builtin hooks that do not specify their own.
    pub stages: Vec<String>,
    /// Global environment merged into every job (issue #2195).
    pub env: BTreeMap<String, String>,
    /// poly's own in-process tools.
    pub builtin: BuiltinHooks,
    /// Per-stage inline hook configuration, keyed by git [`Stage`].
    pub stage_configs: BTreeMap<Stage, StageConfig>,
}

impl HooksConfig {
    /// Validate the parsed configuration. Returns a human-readable error string
    /// describing the first problem found.
    ///
    /// Checks performed:
    /// - Each inline [`Job`] declares exactly one of `run` xor `script`
    ///   (builtins are configured separately under `[hooks.builtin]`, so this
    ///   applies to every inline job without exception).
    /// - `runner` is only meaningful with `script`.
    /// - `skip` and `only` are not both set on the same stage or job.
    ///
    /// Unknown stage keys and imported-repo keys are already rejected during
    /// deserialization, so they never reach this method.
    pub fn validate(&self) -> Result<(), String> {
        for (stage, config) in &self.stage_configs {
            if config.skip.is_some() && config.only.is_some() {
                return Err(format!(
                    "stage `{stage}` sets both `skip` and `only`; choose one"
                ));
            }
            for (label, job) in config.labeled_jobs() {
                validate_job(*stage, &label, job)?;
            }
        }
        Ok(())
    }
}

/// Validate a single inline job within a stage.
fn validate_job(stage: Stage, label: &str, job: &Job) -> Result<(), String> {
    let location = format!("stage `{stage}` job `{label}`");
    match (job.run.is_some(), job.script.is_some()) {
        (true, true) => {
            return Err(format!(
                "{location} sets both `run` and `script`; a job must have exactly one"
            ));
        }
        (false, false) => {
            return Err(format!(
                "{location} has neither `run` nor `script`; a job must have exactly one"
            ));
        }
        _ => {}
    }
    if job.runner.is_some() && job.script.is_none() {
        return Err(format!("{location} sets `runner` without `script`"));
    }
    if job.skip.is_some() && job.only.is_some() {
        return Err(format!(
            "{location} sets both `skip` and `only`; choose one"
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Custom Deserialize: partition reserved keys from per-stage keys.
// ---------------------------------------------------------------------------

impl<'de> Deserialize<'de> for HooksConfig {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(HooksConfigVisitor)
    }
}

struct HooksConfigVisitor;

impl<'de> Visitor<'de> for HooksConfigVisitor {
    type Value = HooksConfig;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a [hooks] table")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut stages: Option<Vec<String>> = None;
        let mut env: Option<BTreeMap<String, String>> = None;
        let mut builtin: Option<BuiltinHooks> = None;
        let mut stage_configs: BTreeMap<Stage, StageConfig> = BTreeMap::new();

        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "stages" => {
                    if stages.replace(map.next_value()?).is_some() {
                        return Err(de::Error::duplicate_field("stages"));
                    }
                }
                "env" => {
                    if env.replace(map.next_value()?).is_some() {
                        return Err(de::Error::duplicate_field("env"));
                    }
                }
                "builtin" => {
                    if builtin.replace(map.next_value()?).is_some() {
                        return Err(de::Error::duplicate_field("builtin"));
                    }
                }
                "repo" | "repos" => {
                    return Err(de::Error::custom(
                        "imported pre-commit repos are no longer supported; \
                         define hooks inline in poly.toml",
                    ));
                }
                other => {
                    let stage = other.parse::<Stage>().map_err(de::Error::custom)?;
                    let config = map.next_value::<StageConfig>()?;
                    if stage_configs.insert(stage, config).is_some() {
                        return Err(de::Error::custom(format!(
                            "duplicate [hooks] stage `{}`",
                            stage.as_str()
                        )));
                    }
                }
            }
        }

        Ok(HooksConfig {
            stages: stages.unwrap_or_default(),
            env: env.unwrap_or_default(),
            builtin: builtin.unwrap_or_default(),
            stage_configs,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(toml: &str) -> HooksConfig {
        toml::from_str(toml).expect("parse [hooks]")
    }

    #[test]
    fn parses_reserved_keys_and_stage_tables() {
        let hooks = parse(
            r#"
stages = ["pre-commit"]
env = { RUST_LOG = "info" }

[builtin]
polylint = true

[pre-commit]
parallel = true
[[pre-commit.jobs]]
run = "cargo fmt --check"

[pre-push.commands.test]
run = "cargo test"
"#,
        );
        assert_eq!(hooks.stages, vec!["pre-commit".to_string()]);
        assert_eq!(hooks.env.get("RUST_LOG").map(String::as_str), Some("info"));
        assert!(hooks.builtin.polylint.enabled);
        assert_eq!(hooks.stage_configs.len(), 2);
        assert!(hooks.stage_configs[&Stage::PreCommit].parallel);
        assert_eq!(hooks.stage_configs[&Stage::PreCommit].jobs.len(), 1);
        assert!(
            hooks.stage_configs[&Stage::PrePush]
                .commands
                .contains_key("test")
        );
        hooks.validate().expect("valid config");
    }

    #[test]
    fn stage_alias_commit_maps_to_pre_commit() {
        let hooks = parse(
            r#"
[commit]
[[commit.jobs]]
run = "echo hi"
"#,
        );
        assert!(hooks.stage_configs.contains_key(&Stage::PreCommit));
    }

    #[test]
    fn unknown_stage_key_is_a_hard_error() {
        let err = toml::from_str::<HooksConfig>(
            r#"
[bogus-stage]
"#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("bogus-stage"), "names the bad key: {err}");
        assert!(err.contains("pre-commit"), "lists valid stages: {err}");
    }

    #[test]
    fn imported_repos_are_rejected() {
        for key in ["repo", "repos"] {
            let err = toml::from_str::<HooksConfig>(&format!("[[{key}]]\n"))
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("no longer supported"),
                "repo key `{key}` rejected with guidance: {err}"
            );
        }
    }

    #[test]
    fn validate_rejects_job_with_both_run_and_script() {
        let hooks = parse(
            r#"
[pre-commit]
[[pre-commit.jobs]]
run = "x"
script = "y.sh"
"#,
        );
        let err = hooks.validate().unwrap_err();
        assert!(err.contains("both `run` and `script`"), "{err}");
    }

    #[test]
    fn validate_rejects_job_with_neither_run_nor_script() {
        let hooks = parse(
            r#"
[pre-commit]
[[pre-commit.jobs]]
tags = ["x"]
"#,
        );
        let err = hooks.validate().unwrap_err();
        assert!(err.contains("neither `run` nor `script`"), "{err}");
    }

    #[test]
    fn validate_rejects_runner_without_script() {
        let hooks = parse(
            r#"
[pre-commit]
[[pre-commit.jobs]]
run = "x"
runner = "bash"
"#,
        );
        let err = hooks.validate().unwrap_err();
        assert!(err.contains("`runner` without `script`"), "{err}");
    }

    #[test]
    fn validate_rejects_skip_and_only_together() {
        let hooks = parse(
            r#"
[pre-commit]
skip = true
only = true
"#,
        );
        let err = hooks.validate().unwrap_err();
        assert!(err.contains("both `skip` and `only`"), "{err}");
    }

    #[test]
    fn validate_rejects_job_skip_and_only_together() {
        let hooks = parse(
            r#"
[pre-commit]
[[pre-commit.jobs]]
run = "x"
skip = true
only = ["merge"]
"#,
        );
        let err = hooks.validate().unwrap_err();
        assert!(err.contains("both `skip` and `only`"), "{err}");
    }
}
