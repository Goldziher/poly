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

use crate::HookSource;
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};

pub use builtin::{BuiltinHook, BuiltinHooks, CargoHooks, DEFAULT_MAX_ADDED_FILE_KB, FileSafetyHooks};
pub use job::{Job, JobCache, Serial};
pub use patterns::{Guard, GuardCondition, GuardMatch, Patterns};
pub use stage::{ParseStageError, Stage};
pub use stage_config::StageConfig;

/// `[hooks]` table — the inline, lefthook-style git-hook configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HooksConfig {
    /// Local or Git producer catalogs selected by this repository.
    pub sources: Vec<HookSource>,
    /// Default stages applied to builtin hooks that do not specify their own.
    pub stages: Vec<String>,
    /// Global environment merged into every job (issue #2195).
    pub env: BTreeMap<String, String>,
    /// poly's own in-process tools.
    pub builtin: BuiltinHooks,
    /// Per-stage inline hook configuration, keyed by git [`Stage`].
    pub stage_configs: BTreeMap<Stage, StageConfig>,
    /// Paths pulled into the staged snapshot from the live worktree even though
    /// git does not track them.
    ///
    /// The snapshot is byte-faithful to the index, which is what makes a commit
    /// gate check the bytes being committed. A `workspace` hook whose build
    /// reads a gitignored input therefore fails under the gate while passing in
    /// the worktree, and the error names the missing file rather than the
    /// isolation that removed it.
    ///
    /// Opt-in and named one path at a time, so the default stays honest and any
    /// deviation from "these are the bytes being committed" is something the
    /// repository wrote down on purpose.
    pub snapshot_include: Patterns,
    /// Whether hooks run against a non-destructive staged-content snapshot
    /// instead of the live worktree.
    ///
    /// Applies to **every** hook in the run — per-file checks as much as
    /// whole-workspace ones (`cargo`, type checkers, …) — so a commit gate never
    /// reports on two different sets of bytes (ADR 0019).
    ///
    /// `None` (unset) uses the default: isolate when a real commit is being
    /// gated (the `pre-commit` git-hook path), but not for `--all-files` / CI
    /// runs, which intentionally check the whole tree. `Some(bool)` forces it.
    pub isolate: Option<bool>,
    /// Whether a `[hooks]` section was present in the source config (vs this
    /// being the default produced when no `[hooks]` table exists). Gates the
    /// default-on builtins (e.g. the `cargo` group) so repos that have not
    /// adopted poly hooks are never silently given extra hooks.
    pub present: bool,
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
    /// - Each `snapshot_include` entry is a repository-relative path.
    ///
    /// Unknown stage keys and imported-repo keys are already rejected during
    /// deserialization, so they never reach this method.
    pub fn validate(&self) -> Result<(), String> {
        crate::hook_sources::validate_sources(&self.sources)?;
        validate_snapshot_include(&self.snapshot_include)?;
        for (stage, config) in &self.stage_configs {
            if config.skip.is_some() && config.only.is_some() {
                return Err(format!("stage `{stage}` sets both `skip` and `only`; choose one"));
            }
            validate_guards(&format!("stage `{stage}`"), config.skip.as_ref(), config.only.as_ref())?;
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
        return Err(format!("{location} sets both `skip` and `only`; choose one"));
    }
    validate_guards(&location, job.skip.as_ref(), job.only.as_ref())?;
    Ok(())
}

/// Reject `skip`/`only` guard conditions poly does not evaluate.
///
/// A guard that parses but is never consulted reads as "this item is scoped"
/// while the item runs unconditionally, which is how an author ends up reaching
/// for a stage-wide `precondition` instead. Failing at load time is the only
/// honest option.
/// Reject a `snapshot_include` entry that is not a repository-relative path.
///
/// Inspected as a string rather than through `std::path`, because `Path` parses
/// per host: `C:\\x` is one component on Unix and an absolute path on Windows,
/// so a `Path`-based check would accept on one platform what it rejects on
/// another. The `extends` `file` key is validated the same way, for the same
/// reason.
///
/// The entry is joined onto the snapshot root, so an absolute path or a `..`
/// segment would write outside it.
fn validate_snapshot_include(patterns: &Patterns) -> Result<(), String> {
    for entry in patterns.iter() {
        let reject = |why: &str| Err(format!("[hooks] snapshot_include entry `{entry}` {why}"));
        if entry.trim().is_empty() {
            return reject("is empty");
        }
        if entry.starts_with('/') || entry.starts_with('\\') {
            return reject("is absolute; entries are relative to the repository root");
        }
        if entry.len() >= 2 && entry.as_bytes()[1] == b':' {
            return reject("names a drive; entries are relative to the repository root");
        }
        if entry.split(['/', '\\']).any(|segment| segment == "..") {
            return reject("escapes the repository with `..`");
        }
    }
    Ok(())
}

fn validate_guards(location: &str, skip: Option<&Guard>, only: Option<&Guard>) -> Result<(), String> {
    for (key, guard) in [("skip", skip), ("only", only)] {
        if let Some(guard) = guard {
            guard
                .validate_supported(key)
                .map_err(|message| format!("{location}: {message}"))?;
        }
    }
    Ok(())
}

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
        let mut isolate: Option<bool> = None;
        let mut snapshot_include: Option<Patterns> = None;
        let mut sources: Option<Vec<HookSource>> = None;
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
                "isolate" => {
                    if isolate.replace(map.next_value()?).is_some() {
                        return Err(de::Error::duplicate_field("isolate"));
                    }
                }
                "snapshot_include" => {
                    if snapshot_include.replace(map.next_value()?).is_some() {
                        return Err(de::Error::duplicate_field("snapshot_include"));
                    }
                }
                "sources" => {
                    if sources.replace(map.next_value()?).is_some() {
                        return Err(de::Error::duplicate_field("sources"));
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
            sources: sources.unwrap_or_default(),
            stages: stages.unwrap_or_default(),
            env: env.unwrap_or_default(),
            builtin: builtin.unwrap_or_default(),
            stage_configs,
            isolate,
            snapshot_include: snapshot_include.unwrap_or_default(),
            present: true,
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
lint = true

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
        assert!(hooks.builtin.lint.enabled);
        assert_eq!(hooks.stage_configs.len(), 2);
        assert!(hooks.stage_configs[&Stage::PreCommit].parallel);
        assert_eq!(hooks.stage_configs[&Stage::PreCommit].jobs.len(), 1);
        assert!(hooks.stage_configs[&Stage::PrePush].commands.contains_key("test"));
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
    fn isolate_defaults_to_unset_and_parses_when_present() {
        assert_eq!(parse("stages = [\"pre-commit\"]\n").isolate, None);
        assert_eq!(parse("isolate = false\n").isolate, Some(false));
        assert_eq!(parse("isolate = true\n").isolate, Some(true));
    }

    #[test]
    fn workspace_flag_parses_on_a_job() {
        let hooks = parse(
            r#"
[pre-commit.scripts.check]
run = "cargo clippy"
workspace = true
"#,
        );
        let job = &hooks.stage_configs[&Stage::PreCommit].scripts["check"];
        assert!(job.workspace, "workspace flag lowered onto the job");
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
    fn job_accepts_a_scoped_precondition_and_before() {
        let hooks = parse(
            r#"
[pre-commit.commands.kotlin]
run = "./gradlew detekt"
precondition = "test -f gradlew"
before = ["./gradlew --version"]
"#,
        );
        hooks.validate().expect("a scoped prerequisite is valid");
        let job = &hooks.stage_configs[&Stage::PreCommit].commands["kotlin"];
        assert_eq!(job.precondition.as_deref(), Some("test -f gradlew"));
        assert_eq!(
            job.before.as_ref().expect("before present").as_slice(),
            &["./gradlew --version".to_string()]
        );
    }

    /// A `{ run = … }` guard is the supported conditional form and must load.
    #[test]
    fn validate_accepts_a_run_guard_condition() {
        let hooks = parse(
            r#"
[pre-commit]
[[pre-commit.jobs]]
name = "kotlin"
run = "x"
only = [{ run = "test -f gradlew" }]
"#,
        );
        hooks.validate().expect("`run` conditions are evaluated");
    }

    /// Guard forms poly does not evaluate are rejected at load time — accepting
    /// and ignoring them makes the author believe an item is scoped when it is
    /// not, which is exactly how a stage-wide guard gets reached for instead.
    #[test]
    fn validate_rejects_guard_conditions_poly_does_not_evaluate() {
        for (guard, expected) in [
            (r#"only = [{ ref = "main" }]"#, "does not evaluate"),
            (r#"skip = ["merge"]"#, "does not evaluate"),
            (r#"skip = [{}]"#, "no `run` command"),
        ] {
            let hooks = parse(&format!(
                r#"
[pre-commit]
[[pre-commit.jobs]]
name = "j"
run = "x"
{guard}
"#
            ));
            let err = hooks.validate().unwrap_err();
            assert!(err.contains(expected), "guard `{guard}` rejected clearly: {err}");
            assert!(err.contains("run = "), "the error names the supported form: {err}");
        }
    }

    /// The same rejection applies to a stage-level guard.
    #[test]
    fn validate_rejects_unsupported_stage_level_guard_conditions() {
        let hooks = parse(
            r#"
[pre-commit]
skip = ["rebase"]
"#,
        );
        let err = hooks.validate().unwrap_err();
        assert!(err.contains("stage `pre-commit`"), "{err}");
        assert!(err.contains("does not evaluate"), "{err}");
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

    fn hooks_from(toml_src: &str) -> HooksConfig {
        toml::from_str(toml_src).expect("valid hooks config")
    }

    #[test]
    fn snapshot_include_accepts_repository_relative_paths() {
        let hooks = hooks_from("snapshot_include = [\"config/local.json\", \"vendor/fixtures\"]\n");
        assert_eq!(hooks.snapshot_include.len(), 2);
        assert!(hooks.validate().is_ok());
    }

    /// Every entry is joined onto the snapshot root, so anything that escapes it
    /// is refused at config load rather than at the join — the reader gets the
    /// error next to the line they wrote.
    #[test]
    fn snapshot_include_refuses_paths_that_escape_the_repository() {
        for entry in [
            "/etc/passwd",
            "../outside.txt",
            "a/../../b",
            "C:/Windows",
            "\\\\server\\share",
        ] {
            let hooks = hooks_from(&format!("snapshot_include = [\"{}\"]\n", entry.replace('\\', "\\\\")));
            let err = hooks.validate().expect_err(&format!("`{entry}` must be refused"));
            assert!(err.contains("snapshot_include"), "{err}");
        }
    }

    #[test]
    fn snapshot_include_defaults_to_empty() {
        assert!(hooks_from("isolate = true\n").snapshot_include.is_empty());
    }
}
