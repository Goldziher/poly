//! The `poly.toml` `timeout` key, end to end: config → lowering → a real kill.
//!
//! A key that parses and is then ignored is a false promise, so these tests
//! stop at nothing short of the hook actually being killed at the budget the
//! config asked for.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use poly_config::{HookCacheMode, HooksConfig, PolyConfig, ToolsConfig};
use poly_hooks::model::HookStatus;
use poly_hooks::timeout::{HookTimeout, budget_for};
use poly_hooks::{Hook, HookRunReporter, HookRunRequest, Stage, StageSpec, run};
use poly_workspace::lower::lower_stage;
use tempfile::TempDir;

/// Write `toml` as a `poly.toml` in a fresh directory and load it.
fn config(toml: &str) -> (TempDir, HooksConfig) {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("poly.toml");
    std::fs::write(&path, toml).expect("write poly.toml");
    let hooks = PolyConfig::load_file(&path).expect("load poly.toml").hooks;
    (dir, hooks)
}

fn lower(root: &Path, hooks: &HooksConfig) -> anyhow::Result<StageSpec> {
    lower_stage(
        hooks,
        &PathBuf::from("/opt/poly/bin/poly"),
        Stage::PreCommit,
        &[],
        &HookCacheMode::default(),
        root,
        &ToolsConfig::default(),
    )
}

fn hook_named<'a>(spec: &'a StageSpec, id: &str) -> &'a Hook {
    spec.hooks.iter().find(|hook| hook.id == id).unwrap_or_else(|| {
        panic!(
            "no hook `{id}` in {:?}",
            spec.hooks.iter().map(|h| &h.id).collect::<Vec<_>>()
        )
    })
}

/// A job whose `run` is `sleep 30` under a sub-second budget: it can only ever
/// end by being killed.
fn wedged_config(timeout: &str) -> String {
    format!(
        r#"
[hooks.builtin]
lint = false
fmt = false

[[hooks.pre-commit.jobs]]
name = "wedged"
run = "sleep 30"
timeout = {timeout}
"#
    )
}

/// The whole chain: a `timeout` in `poly.toml` lowers onto the hook and the
/// runner kills the hook at exactly that budget.
#[test]
fn a_timeout_in_poly_toml_kills_the_hook_at_that_budget() {
    let (dir, hooks) = config(&wedged_config(r#""200ms""#));
    let root = dir.path();
    let spec = lower(root, &hooks).expect("lower");

    assert_eq!(
        hook_named(&spec, "wedged").timeout,
        HookTimeout::Limit(Duration::from_millis(200)),
        "the config value must reach the hook"
    );

    let outcome = run(HookRunRequest {
        root: root.to_path_buf(),
        stages: vec![spec],
        ..HookRunRequest::default()
    })
    .expect("run");

    assert!(!outcome.success(), "a killed hook fails the run");
    let hook = outcome.stages[0]
        .hooks
        .iter()
        .find(|hook| hook.id == "wedged")
        .expect("the wedged hook ran");
    let HookStatus::TimedOut(reason) = &hook.status else {
        panic!("expected a timeout, got {:?}", hook.status);
    };
    assert_eq!(
        reason.limit,
        Duration::from_millis(200),
        "the configured budget applied"
    );
    assert!(reason.elapsed >= Duration::from_millis(200));
    assert!(
        hook.duration < Duration::from_secs(30),
        "the runner must not have waited for `sleep 30`"
    );

    let report = HookRunReporter::new().render(&outcome);
    assert!(
        report.contains("wedged (timed out: poly killed it after"),
        "the report names the hook and the kill: {report}"
    );
    assert!(report.contains("limit 200ms"), "the report names the budget: {report}");
}

/// Every accepted form lowers to the duration it reads as — including a bare
/// integer, which is seconds, matching the environment variables.
#[test]
fn every_accepted_timeout_form_lowers_to_its_duration() {
    for (value, expected) in [
        (r#""500ms""#, Duration::from_millis(500)),
        (r#""45s""#, Duration::from_secs(45)),
        ("90", Duration::from_secs(90)),
        (r#""10m""#, Duration::from_mins(10)),
        (r#""1h""#, Duration::from_hours(1)),
    ] {
        let (dir, hooks) = config(&wedged_config(value));
        let spec = lower(dir.path(), &hooks).expect("lower");
        assert_eq!(
            hook_named(&spec, "wedged").timeout,
            HookTimeout::Limit(expected),
            "`timeout = {value}`"
        );
    }
}

/// The disable forms disable, exactly as they do in the environment: no limit,
/// and back to the un-supervised execution path.
#[test]
fn the_disable_forms_disable_the_budget_from_config() {
    for value in ["0", r#""0""#, r#""off""#, r#""none""#] {
        let (dir, hooks) = config(&wedged_config(value));
        let spec = lower(dir.path(), &hooks).expect("lower");
        let hook = hook_named(&spec, "wedged");
        assert_eq!(hook.timeout, HookTimeout::Disabled, "`timeout = {value}`");

        let budget = budget_for(hook);
        assert_eq!(budget.limit, None, "`timeout = {value}` must remove the limit");
        assert!(
            !budget.is_supervised(),
            "`timeout = {value}` must restore the un-supervised path"
        );
    }
}

/// A job that says nothing about timeouts keeps the shape-derived default.
#[test]
fn a_job_without_a_timeout_keeps_the_shape_default() {
    let (dir, hooks) = config(
        r#"
[hooks.builtin]
lint = false
fmt = false

[[hooks.pre-commit.jobs]]
name = "quiet"
run = "true"
"#,
    );
    let spec = lower(dir.path(), &hooks).expect("lower");
    let hook = hook_named(&spec, "quiet");
    assert_eq!(hook.timeout, HookTimeout::Default);
    assert_eq!(budget_for(hook).limit, Some(poly_hooks::timeout::DEFAULT_HOOK_TIMEOUT));
}

/// A value poly cannot read is a hard error naming the hook and spelling out
/// what it would have accepted — silently ignoring it would leave the author
/// believing a budget applies when none does.
#[test]
fn a_malformed_timeout_names_the_hook_and_the_accepted_forms() {
    let (dir, hooks) = config(&wedged_config(r#""soon""#));
    let error = lower(dir.path(), &hooks).expect_err("a malformed timeout must fail lowering");
    assert_eq!(
        error.to_string(),
        "hook job `wedged` in stage `pre-commit`: invalid `timeout` value `soon`; expected whole seconds (`90`), \
         a duration (`500ms`, `30s`, `10m`, `1h`), or `0`/`off`/`none` to disable"
    );
}
