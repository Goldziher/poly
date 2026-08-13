//! Unit tests for [`super`]: stage mapping and `[hooks]` lowering.

use super::*;
use poly_config::PolyConfig;

fn hooks_from(toml: &str) -> HooksConfig {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("poly.toml");
    std::fs::write(&path, toml).unwrap();
    PolyConfig::load_file(&path).unwrap().hooks
}

fn poly() -> PathBuf {
    PathBuf::from("/opt/poly/bin/poly")
}

fn ids(spec: &StageSpec) -> Vec<String> {
    spec.hooks.iter().map(|h| h.id.clone()).collect()
}

/// Test-local `lower_stage` shadowing the production one (a local item wins
/// over the `use super::*` glob): it routes lowering through a probe that
/// reports no external tools, so the default-on `cargo` builtin group stays
/// deterministic regardless of what the host has installed.
fn lower_stage(
    hooks: &HooksConfig,
    poly_bin: &Path,
    stage: HookStage,
    files: &[PathBuf],
    cache_mode: &HookCacheMode,
) -> Result<StageSpec> {
    struct NoTools;
    impl ToolProbe for NoTools {
        fn is_available(&self, _tool: &str) -> bool {
            false
        }
        fn is_cargo_project(&self) -> bool {
            false
        }
        fn guard_passes(&self, _command: &str) -> bool {
            true
        }
    }
    lower_stage_with_probe(
        hooks,
        poly_bin,
        stage,
        files,
        cache_mode,
        &NoTools,
        &poly_config::ToolsConfig::default(),
    )
}

#[test]
fn to_and_from_hook_stage_round_trip_for_every_runner_stage() {
    use clap::ValueEnum as _;
    for stage in HookStage::value_variants() {
        let config_stage = from_hook_stage(*stage);
        assert_eq!(
            to_hook_stage(config_stage),
            Some(*stage),
            "round-trip failed for {stage:?}"
        );
    }
}

#[test]
fn always_pseudo_stage_has_no_runner_counterpart() {
    assert_eq!(to_hook_stage(ConfigStage::Always), None);
}

#[test]
fn every_config_stage_maps_to_a_runner_stage_except_always() {
    for stage in ConfigStage::ALL {
        match stage {
            ConfigStage::Always => assert_eq!(to_hook_stage(*stage), None),
            other => assert!(
                to_hook_stage(*other).is_some(),
                "config stage {other} has no runner counterpart"
            ),
        }
    }
}

#[test]
fn legacy_stage_aliases_agree_across_both_enums() {
    for (alias, expected) in [
        ("commit", HookStage::PreCommit),
        ("push", HookStage::PrePush),
        ("merge-commit", HookStage::PreMergeCommit),
    ] {
        let config_stage: ConfigStage = alias.parse().expect("config alias parses");
        assert_eq!(to_hook_stage(config_stage), Some(expected));

        let runner_stage: HookStage =
            serde_json::from_value(serde_json::Value::String(alias.to_string())).expect("runner alias parses");
        assert_eq!(runner_stage, expected);
    }
}

#[test]
fn builtins_only_lowers_to_poly_entries() {
    let hooks = hooks_from(
        r#"
[hooks]
stages = ["pre-commit"]
[hooks.builtin]
lint = true
fmt = true
"#,
    );
    let spec = lower_stage(&hooks, &poly(), HookStage::PreCommit, &[], &HookCacheMode::Safe).unwrap();
    assert_eq!(ids(&spec), vec!["lint", "fmt"]);
    let HookCommand::Run(line) = &spec.hooks[0].command else {
        panic!("expected run command");
    };
    // `--force-exclude` is not optional here: a hook is handed staged paths
    // rather than deliberately named ones, so without it the repo's
    // `[discovery] exclude` is silently inert in the one place it matters most.
    assert!(
        line.ends_with(" lint --no-workspace --force-exclude"),
        "unexpected line: {line}"
    );
    assert!(line.contains("/opt/poly/bin/poly"));
    assert!(spec.hooks[0].pass_filenames);
}

#[test]
fn builtin_exclude_lowers_to_the_hook_exclude_glob() {
    let hooks = hooks_from(
        r#"
[hooks.builtin]
lint = { exclude = ["**/tags.rs", ".ai-rulez/**"] }
fmt = { exclude = "crates/*/tests/fixtures/**" }
"#,
    );
    let spec = lower_stage(&hooks, &poly(), HookStage::PreCommit, &[], &HookCacheMode::Safe).unwrap();
    let lint = spec.hooks.iter().find(|h| h.id == "lint").unwrap();
    let lint_exclude = lint.exclude.as_ref().expect("lint exclude present");
    assert!(lint_exclude.is_match(Path::new("crates/poly-hooks/src/identify/tags.rs")));
    assert!(lint_exclude.is_match(Path::new(".ai-rulez/foo.md")));
    assert!(!lint_exclude.is_match(Path::new("crates/poly-cli/src/main.rs")));

    let fmt = spec.hooks.iter().find(|h| h.id == "fmt").unwrap();
    let fmt_exclude = fmt.exclude.as_ref().expect("fmt exclude present");
    assert!(fmt_exclude.is_match(Path::new("crates/poly-core/tests/fixtures/bad.md")));
    assert!(!fmt_exclude.is_match(Path::new("crates/poly-core/src/lib.rs")));
}

/// `[discovery] exclude` is repo-wide policy, so every file-scoped builtin
/// inherits it: a repo states its excluded paths once instead of pasting the
/// same list into `[discovery]`, `lint`, `fmt`, and `file_safety`.
#[test]
fn builtin_hooks_inherit_the_discovery_exclude_globs() {
    let hooks = hooks_from(
        r#"
[discovery]
exclude = ["vendor/**"]

[hooks.builtin]
lint = { exclude = ["**/tags.rs"] }
fmt = true
file_safety = true
"#,
    );
    let spec = lower_stage(&hooks, &poly(), HookStage::PreCommit, &[], &HookCacheMode::Safe).unwrap();
    for id in ["lint", "fmt", "file-safety"] {
        let hook = spec.hooks.iter().find(|h| h.id == id).unwrap();
        let exclude = hook.exclude.as_ref().unwrap_or_else(|| panic!("{id} exclude present"));
        assert!(exclude.is_match(Path::new("vendor/lib/x.rs")), "{id} excludes vendor");
        assert!(!exclude.is_match(Path::new("crates/poly-cli/src/main.rs")), "{id}");
    }
    let lint = spec.hooks.iter().find(|h| h.id == "lint").unwrap();
    let lint_exclude = lint.exclude.as_ref().expect("lint exclude present");
    assert!(
        lint_exclude.is_match(Path::new("crates/poly-hooks/src/identify/tags.rs")),
        "the hook's own glob survives alongside the inherited one"
    );
}

#[test]
fn builtin_hook_exclude_mode_replace_keeps_only_its_own_globs() {
    let hooks = hooks_from(
        r#"
[discovery]
exclude = ["vendor/**"]

[hooks.builtin.lint]
exclude = ["**/tags.rs"]
exclude_mode = "replace"
"#,
    );
    let spec = lower_stage(&hooks, &poly(), HookStage::PreCommit, &[], &HookCacheMode::Safe).unwrap();
    let lint = spec.hooks.iter().find(|h| h.id == "lint").unwrap();
    let exclude = lint.exclude.as_ref().expect("lint exclude present");
    assert!(exclude.is_match(Path::new("crates/poly-hooks/src/identify/tags.rs")));
    assert!(
        !exclude.is_match(Path::new("vendor/lib/x.rs")),
        "opted out of inheriting"
    );
}

#[test]
fn commit_builtin_defaults_to_commit_msg_stage() {
    let hooks = hooks_from(
        r#"
[hooks.builtin]
commit = true
"#,
    );
    let pre = lower_stage(&hooks, &poly(), HookStage::PreCommit, &[], &HookCacheMode::Safe).unwrap();
    assert!(pre.hooks.is_empty());
    let msg = lower_stage(&hooks, &poly(), HookStage::CommitMsg, &[], &HookCacheMode::Safe).unwrap();
    assert_eq!(ids(&msg), vec!["poly-commit"]);
    assert!(msg.hooks[0].pass_filenames);
    assert!(matches!(msg.hooks[0].stage, HookStage::CommitMsg));
}

#[test]
fn inline_jobs_carry_run_args_env_and_stage_fixed() {
    let hooks = hooks_from(
        r#"
[hooks]
env = { GLOBAL = "1" }
[hooks.pre-commit]
parallel = true
[[hooks.pre-commit.jobs]]
name = "fmt"
run = "cargo fmt"
args = ["--check"]
env = { LOCAL = "2" }
stage_fixed = true
files = "**/*.rs"
"#,
    );
    let spec = lower_stage(&hooks, &poly(), HookStage::PreCommit, &[], &HookCacheMode::Safe).unwrap();
    assert_eq!(ids(&spec), vec!["fmt"]);
    let hook = &spec.hooks[0];
    assert!(matches!(&hook.command, HookCommand::Run(line) if line == "cargo fmt"));
    assert_eq!(hook.args, vec!["--check".to_string()]);
    assert_eq!(hook.env.get("GLOBAL").map(String::as_str), Some("1"));
    assert_eq!(hook.env.get("LOCAL").map(String::as_str), Some("2"));
    assert!(hook.stage_fixed);
    assert!(hook.parallel);
    assert!(hook.files.is_some());
    assert!(!hook.always_run);
}

#[test]
fn script_job_maps_to_script_command() {
    let hooks = hooks_from(
        r#"
[hooks.pre-commit.scripts.lint]
script = "lint.sh"
runner = "bash"
"#,
    );
    let spec = lower_stage(&hooks, &poly(), HookStage::PreCommit, &[], &HookCacheMode::Safe).unwrap();
    let HookCommand::Script { path, runner } = &spec.hooks[0].command else {
        panic!("expected script command");
    };
    assert_eq!(path, "lint.sh");
    assert_eq!(runner.as_deref(), Some("bash"));
}

#[test]
fn builtins_and_inline_jobs_are_priority_ordered() {
    let hooks = hooks_from(
        r#"
[hooks.builtin]
lint = { stages = ["pre-commit"] }
[hooks.pre-commit]
[[hooks.pre-commit.jobs]]
name = "early"
run = "echo early"
priority = -5
[[hooks.pre-commit.jobs]]
name = "late"
run = "echo late"
priority = 5
"#,
    );
    let spec = lower_stage(&hooks, &poly(), HookStage::PreCommit, &[], &HookCacheMode::Safe).unwrap();
    assert_eq!(ids(&spec), vec!["early", "lint", "late"]);
}

#[test]
fn always_jobs_are_appended_to_multiple_stages() {
    let hooks = hooks_from(
        r#"
[hooks.always]
[[hooks.always.jobs]]
name = "everywhere"
run = "echo hi"
[hooks.pre-commit]
[[hooks.pre-commit.jobs]]
name = "commit-only"
run = "echo commit"
"#,
    );
    let pre = lower_stage(&hooks, &poly(), HookStage::PreCommit, &[], &HookCacheMode::Safe).unwrap();
    assert_eq!(ids(&pre), vec!["commit-only", "everywhere"]);
    let push = lower_stage(&hooks, &poly(), HookStage::PrePush, &[], &HookCacheMode::Safe).unwrap();
    assert_eq!(ids(&push), vec!["everywhere"]);
}

#[test]
fn patterns_lower_to_file_and_exclude_globs() {
    let hooks = hooks_from(
        r#"
[hooks.pre-commit]
[[hooks.pre-commit.jobs]]
run = "x"
files = ["src/**/*.rs"]
exclude = "src/generated/**"
"#,
    );
    let spec = lower_stage(&hooks, &poly(), HookStage::PreCommit, &[], &HookCacheMode::Safe).unwrap();
    let hook = &spec.hooks[0];
    assert!(hook.files.as_ref().unwrap().is_match(Path::new("src/a.rs")));
    assert!(hook.exclude.as_ref().unwrap().is_match(Path::new("src/generated/x.rs")));
}

#[test]
fn unconditional_skip_drops_a_job_and_only_false_too() {
    let hooks = hooks_from(
        r#"
[hooks.pre-commit]
[[hooks.pre-commit.jobs]]
name = "skipped"
run = "x"
skip = true
[[hooks.pre-commit.jobs]]
name = "only-false"
run = "y"
only = false
[[hooks.pre-commit.jobs]]
name = "kept"
run = "z"
"#,
    );
    let spec = lower_stage(&hooks, &poly(), HookStage::PreCommit, &[], &HookCacheMode::Safe).unwrap();
    assert_eq!(ids(&spec), vec!["kept"]);
}

#[test]
fn stage_skip_yields_empty_stage() {
    let hooks = hooks_from(
        r#"
[hooks.pre-commit]
skip = true
[[hooks.pre-commit.jobs]]
run = "x"
"#,
    );
    let spec = lower_stage(&hooks, &poly(), HookStage::PreCommit, &[], &HookCacheMode::Safe).unwrap();
    assert!(spec.hooks.is_empty());
}

#[test]
fn template_tokens_are_substituted_and_disable_file_passing() {
    let hooks = hooks_from(
        r#"
[hooks.pre-commit]
[[hooks.pre-commit.jobs]]
run = "prettier --write {staged_files}"
"#,
    );
    let files = vec![PathBuf::from("a.js"), PathBuf::from("b.js")];
    let spec = lower_stage(&hooks, &poly(), HookStage::PreCommit, &files, &HookCacheMode::Safe).unwrap();
    let hook = &spec.hooks[0];
    let HookCommand::Run(line) = &hook.command else {
        panic!("expected run command");
    };
    assert!(line.contains("a.js") && line.contains("b.js"), "{line}");
    assert!(!hook.pass_filenames);
}

#[test]
fn template_substitution_is_scoped_by_the_job_glob() {
    let hooks = hooks_from(
        r#"
[hooks.pre-commit]
[[hooks.pre-commit.jobs]]
run = "cargo fmt {staged_files}"
files = "**/*.rs"
"#,
    );
    let files = vec![PathBuf::from("src/a.rs"), PathBuf::from("README.md")];
    let spec = lower_stage(&hooks, &poly(), HookStage::PreCommit, &files, &HookCacheMode::Safe).unwrap();
    let HookCommand::Run(line) = &spec.hooks[0].command else {
        panic!("expected run command");
    };
    assert!(line.contains("a.rs"), "{line}");
    assert!(!line.contains("README.md"), "non-matching file leaked: {line}");
}

#[test]
#[cfg(not(windows))]
fn template_substitution_shell_quotes_paths_with_spaces() {
    let hooks = hooks_from(
        r#"
[hooks.pre-commit]
[[hooks.pre-commit.jobs]]
run = "echo {staged_files}"
"#,
    );
    let files = vec![PathBuf::from("my file.js")];
    let spec = lower_stage(&hooks, &poly(), HookStage::PreCommit, &files, &HookCacheMode::Safe).unwrap();
    let HookCommand::Run(line) = &spec.hooks[0].command else {
        panic!("expected run command");
    };
    assert!(line.contains("'my file.js'"), "unquoted path: {line}");
}

#[test]
fn stage_skip_suppresses_builtins_too() {
    let hooks = hooks_from(
        r#"
[hooks.builtin]
lint = true
[hooks.pre-commit]
skip = true
"#,
    );
    let spec = lower_stage(&hooks, &poly(), HookStage::PreCommit, &[], &HookCacheMode::Safe).unwrap();
    assert!(spec.hooks.is_empty(), "stage skip must suppress builtins too");
}

#[test]
fn stage_precondition_before_after_carry_over() {
    let hooks = hooks_from(
        r#"
[hooks.pre-commit]
precondition = "command -v cargo"
before = "echo before"
after = ["echo a", "echo b"]
[[hooks.pre-commit.jobs]]
run = "x"
"#,
    );
    let spec = lower_stage(&hooks, &poly(), HookStage::PreCommit, &[], &HookCacheMode::Safe).unwrap();
    assert_eq!(spec.precondition.as_deref(), Some("command -v cargo"));
    assert_eq!(spec.before, vec!["echo before"]);
    assert_eq!(spec.after, vec!["echo a", "echo b"]);
}

#[test]
fn exclude_tags_drop_matching_jobs() {
    let hooks = hooks_from(
        r#"
[hooks.pre-commit]
exclude_tags = ["slow"]
[[hooks.pre-commit.jobs]]
name = "fast"
run = "x"
tags = ["quick"]
[[hooks.pre-commit.jobs]]
name = "slow-job"
run = "y"
tags = ["slow"]
"#,
    );
    let spec = lower_stage(&hooks, &poly(), HookStage::PreCommit, &[], &HookCacheMode::Safe).unwrap();
    assert_eq!(ids(&spec), vec!["fast"]);
}

/// `lower_stage` over a probe whose `skip`/`only` guard conditions all resolve
/// to `active`, so guard evaluation is deterministic without spawning a shell.
fn lower_stage_with_guard(hooks: &HooksConfig, guard_result: bool) -> Result<StageSpec> {
    struct GuardProbe(bool);
    impl ToolProbe for GuardProbe {
        fn is_available(&self, _tool: &str) -> bool {
            false
        }
        fn is_cargo_project(&self) -> bool {
            false
        }
        fn guard_passes(&self, _command: &str) -> bool {
            self.0
        }
    }
    lower_stage_with_probe(
        hooks,
        &poly(),
        HookStage::PreCommit,
        &[],
        &HookCacheMode::Safe,
        &GuardProbe(guard_result),
        &poly_config::ToolsConfig::default(),
    )
}

/// A job's `precondition`/`before` reach the runner model, so a prerequisite can
/// be scoped to the hook it guards instead of the whole stage.
#[test]
fn job_precondition_and_before_lower_onto_the_hook() {
    let hooks = hooks_from(
        r#"
[hooks.pre-commit.commands.kotlin]
run = "./gradlew detekt"
precondition = "test -f gradlew"
before = ["./gradlew --version", "echo ready"]
"#,
    );
    let spec = lower_stage(&hooks, &poly(), HookStage::PreCommit, &[], &HookCacheMode::Safe).unwrap();
    let kotlin = spec.hooks.iter().find(|h| h.id == "kotlin").expect("kotlin job");
    assert_eq!(kotlin.precondition.as_deref(), Some("test -f gradlew"));
    assert_eq!(kotlin.before, vec!["./gradlew --version", "echo ready"]);
    // The stage-wide steps stay empty — the prerequisite is scoped to one hook.
    assert_eq!(spec.precondition, None);
    assert!(spec.before.is_empty());
}

/// `only = [{ run = … }]` is evaluated, not ignored: an inactive condition drops
/// the job.
#[test]
fn inactive_only_run_condition_drops_the_job() {
    let hooks = hooks_from(
        r#"
[hooks.pre-commit.commands.kotlin]
run = "./gradlew detekt"
only = [{ run = "test -d /nonexistent" }]
"#,
    );
    let spec = lower_stage_with_guard(&hooks, false).unwrap();
    assert!(ids(&spec).is_empty(), "an inactive `only` guard must drop the job");
}

/// The same guard, active, keeps the job.
#[test]
fn active_only_run_condition_keeps_the_job() {
    let hooks = hooks_from(
        r#"
[hooks.pre-commit.commands.kotlin]
run = "./gradlew detekt"
only = [{ run = "test -d ." }]
"#,
    );
    let spec = lower_stage_with_guard(&hooks, true).unwrap();
    assert_eq!(ids(&spec), vec!["kotlin"]);
}

/// An active `skip = [{ run = … }]` drops the job; inactive keeps it.
#[test]
fn skip_run_condition_is_evaluated_in_both_directions() {
    let hooks = hooks_from(
        r#"
[hooks.pre-commit.commands.kotlin]
run = "./gradlew detekt"
skip = [{ run = "test -f .skip-kotlin" }]
"#,
    );
    assert!(ids(&lower_stage_with_guard(&hooks, true).unwrap()).is_empty());
    assert_eq!(ids(&lower_stage_with_guard(&hooks, false).unwrap()), vec!["kotlin"]);
}

/// A stage-level `only` condition is evaluated too — an inactive one empties the
/// stage rather than being ignored.
#[test]
fn stage_level_only_run_condition_is_evaluated() {
    let hooks = hooks_from(
        r#"
[hooks.pre-commit]
only = [{ run = "test -d /nonexistent" }]
[[hooks.pre-commit.jobs]]
name = "j"
run = "x"
"#,
    );
    assert!(ids(&lower_stage_with_guard(&hooks, false).unwrap()).is_empty());
    assert_eq!(ids(&lower_stage_with_guard(&hooks, true).unwrap()), vec!["j"]);
}

/// The `serial` key resolves to an exclusion set: `true` joins the shared one,
/// a string joins the named one, and `false` opts out of both — including out
/// of a stage-level `parallel = false` and the automatic cargo grouping.
#[test]
fn serial_key_resolves_to_an_exclusion_set() {
    let hooks = hooks_from(
        r#"
[hooks.pre-commit.commands.shared]
run = "./slow-tool"
serial = true

[hooks.pre-commit.commands.named]
run = "./other-tool"
serial = "npm"

[hooks.pre-commit.commands.opted-out]
run = "cargo test"
serial = false
"#,
    );
    let spec = lower_stage(&hooks, &poly(), HookStage::PreCommit, &[], &HookCacheMode::Safe).unwrap();
    let group = |id: &str| {
        spec.hooks
            .iter()
            .find(|hook| hook.id == id)
            .unwrap_or_else(|| panic!("hook {id} lowered"))
            .serial_group
            .clone()
    };

    assert_eq!(group("shared").as_deref(), Some(SHARED_SERIAL_GROUP));
    assert_eq!(group("named").as_deref(), Some("npm"));
    assert_eq!(
        group("opted-out"),
        None,
        "`serial = false` opts out of the automatic cargo grouping too"
    );
    for hook in &spec.hooks {
        assert!(hook.parallel, "{} must still run beside hooks outside its set", hook.id);
    }
}

/// A job that invokes cargo joins the cargo set without being told to — the
/// lock it would otherwise block on is not optional.
#[test]
fn a_job_invoking_cargo_joins_the_cargo_set() {
    let hooks = hooks_from(
        r#"
[hooks.pre-commit.commands.tests]
run = "cargo test --workspace"
workspace = true

[hooks.pre-commit.commands.piped]
run = "make check && cargo build"
workspace = true

[hooks.pre-commit.commands.unrelated]
run = "pnpm tsc --noEmit"
workspace = true
"#,
    );
    let spec = lower_stage(&hooks, &poly(), HookStage::PreCommit, &[], &HookCacheMode::Safe).unwrap();
    let group = |id: &str| {
        spec.hooks
            .iter()
            .find(|hook| hook.id == id)
            .unwrap_or_else(|| panic!("hook {id} lowered"))
            .serial_group
            .clone()
    };

    assert_eq!(group("tests").as_deref(), Some(CARGO_SERIAL_GROUP));
    assert_eq!(
        group("piped").as_deref(),
        Some(CARGO_SERIAL_GROUP),
        "cargo behind a `&&` still takes the same lock"
    );
    assert_eq!(group("unrelated"), None, "a non-cargo tool is not constrained");
}

/// A whole-project job runs concurrently by default: overlapping whole-project
/// tools is where a run's wall-clock is won, and the stage-level `parallel`
/// flag (default `false`) was never about them.
#[test]
fn a_whole_project_job_is_concurrent_by_default() {
    let hooks = hooks_from(
        r#"
[hooks.pre-commit.commands.types]
run = "pyrefly check"
workspace = true

[hooks.pre-commit.commands.per-file]
run = "./format-one"
files = "**/*.py"
"#,
    );
    let spec = lower_stage(&hooks, &poly(), HookStage::PreCommit, &[], &HookCacheMode::Safe).unwrap();
    let hook = |id: &str| {
        spec.hooks
            .iter()
            .find(|hook| hook.id == id)
            .unwrap_or_else(|| panic!("hook {id} lowered"))
    };

    assert!(hook("types").parallel, "a whole-project job is concurrent by default");
    assert!(hook("types").serial_group.is_none());
    assert!(
        !hook("per-file").parallel,
        "a per-file job still follows the stage's `parallel` flag"
    );
}
