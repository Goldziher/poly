//! Per-job scoping for the `[hooks.builtin]` tools.
//!
//! A builtin carries one `files` / `exclude` pair, which is the wrong shape when
//! several source jobs (a lefthook job, a pre-commit hook) map onto the same
//! tool with different scopes: merging them either widens an exclude or drops
//! one. `jobs` keeps each scope as its own hook instead. Each entry lowers to
//! one hook, and any key it leaves out falls back to the builtin's own.

use serde::Deserialize;

use super::patterns::Patterns;

/// One scoped run of the `lint` or `fmt` builtin.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct BuiltinJob {
    /// Hook name shown in the report; defaults to `<tool>-<n>`.
    pub name: Option<String>,
    /// Stages this job runs in; empty inherits the builtin's `stages`.
    pub stages: Vec<String>,
    /// File include glob(s); unset inherits the builtin's `files`.
    pub files: Option<Patterns>,
    /// File exclude glob(s); unset inherits the builtin's `exclude`.
    pub exclude: Option<Patterns>,
    /// Apply fixes and re-stage them; unset inherits the builtin's `fix`.
    pub fix: Option<bool>,
    /// Lower values run first; unset inherits the builtin's `priority`.
    pub priority: Option<i64>,
}

/// One scoped run of a subset of the `file_safety` checks.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct FileSafetyJob {
    /// Hook name shown in the report; defaults to `file-safety-<n>`.
    pub name: Option<String>,
    /// Stages this job runs in; empty inherits the group's `stages`.
    pub stages: Vec<String>,
    /// The member checks this job runs (`merge_conflict`, `private_key`, ...).
    /// Empty runs every enabled member. A listed member must be enabled.
    pub checks: Vec<String>,
    /// File include glob(s); unset inherits the group's `files`.
    pub files: Option<Patterns>,
    /// File exclude glob(s); unset inherits the group's `exclude`.
    pub exclude: Option<Patterns>,
    /// Lower values run first; unset inherits the group's `priority`.
    pub priority: Option<i64>,
}

/// Every `file_safety` member check, in the order poly documents them.
pub const FILE_SAFETY_CHECKS: &[&str] = &[
    "merge_conflict",
    "added_large_files",
    "private_key",
    "case_conflict",
    "executables_have_shebangs",
    "shebang_scripts_are_executable",
    "aws_credentials",
    "requirements_txt_fixer",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_job_may_set_any_subset_of_keys() {
        let job: BuiltinJob = toml::from_str("files = \"*.py\"\nfix = true").unwrap();
        assert_eq!(job.fix, Some(true));
        assert!(job.exclude.is_none());
        assert!(job.priority.is_none());
    }

    fn validated(toml: &str) -> Result<(), String> {
        toml::from_str::<crate::HooksConfig>(toml).unwrap().validate()
    }

    #[test]
    fn jobs_and_fix_parse_on_lint_and_fmt() {
        let hooks: crate::BuiltinHooks = toml::from_str(
            r#"
lint = { fix = true, priority = 2, jobs = [{ files = "*.py" }, { name = "js", exclude = "gen/**", fix = false }] }
fmt = { jobs = [{ stages = ["pre-push"] }] }
"#,
        )
        .unwrap();
        assert!(hooks.lint.fix);
        assert_eq!(hooks.lint.priority, 2);
        assert_eq!(hooks.lint.jobs.len(), 2);
        assert_eq!(hooks.lint.jobs[1].fix, Some(false));
        assert_eq!(hooks.fmt.jobs[0].stages, ["pre-push"]);
    }

    #[test]
    fn commit_takes_neither_fix_nor_jobs() {
        assert!(validated("[builtin]\ncommit = { fix = true }").is_err());
        assert!(validated("[builtin]\ncommit = { jobs = [{}] }").is_err());
        assert!(validated("[builtin]\ncommit = true").is_ok());
    }

    #[test]
    fn a_file_safety_job_may_only_name_enabled_known_checks() {
        let unknown = validated("[builtin.file_safety]\njobs = [{ checks = [\"typo\"] }]").unwrap_err();
        assert!(unknown.contains("unknown check `typo`"), "{unknown}");
        let disabled = validated("[builtin.file_safety]\nprivate_key = false\njobs = [{ checks = [\"private_key\"] }]")
            .unwrap_err();
        assert!(disabled.contains("`private_key`, which is disabled"), "{disabled}");
        let off_by_default =
            validated("[builtin.file_safety]\njobs = [{ checks = [\"aws_credentials\"] }]").unwrap_err();
        assert!(off_by_default.contains("aws_credentials"), "{off_by_default}");
        assert!(
            validated("[builtin.file_safety]\naws_credentials = true\njobs = [{ checks = [\"aws_credentials\"] }]")
                .is_ok()
        );
    }

    #[test]
    fn aws_credentials_is_opt_in_even_when_the_group_is_on() {
        let on: crate::BuiltinHooks = toml::from_str("file_safety = true").unwrap();
        assert!(on.file_safety.merge_conflict && !on.file_safety.aws_credentials);
    }

    #[test]
    fn requirements_txt_fixer_is_opt_in_even_when_the_group_is_on() {
        let on: crate::BuiltinHooks = toml::from_str("file_safety = true").unwrap();
        assert!(!on.file_safety.requirements_txt_fixer);
        let named: crate::BuiltinHooks = toml::from_str("file_safety = { requirements_txt_fixer = true }").unwrap();
        assert!(named.file_safety.check_enabled("requirements_txt_fixer"));
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(toml::from_str::<BuiltinJob>("glob = \"*.py\"").is_err());
        assert!(toml::from_str::<FileSafetyJob>("check = [\"private_key\"]").is_err());
    }
}
