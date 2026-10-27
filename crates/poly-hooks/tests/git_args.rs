//! The arguments git passes to a hook reach the hook's jobs: as `$@` for stages
//! with no file list, in place of the bare message file for message stages, and
//! as `POLY_GIT_ARG_*` in every hook's environment.

#![cfg(unix)]

use std::path::Path;

use poly_hooks::{Hook, HookRunRequest, Stage, StageSpec, run};
use tempfile::TempDir;

fn stage_of(stage: Stage, hook: Hook) -> StageSpec {
    StageSpec {
        stage,
        hooks: vec![Hook { stage, ..hook }],
        ..StageSpec::default()
    }
}

fn run_with_args(root: &Path, stage: Stage, hook: Hook, git_args: &[&str]) -> String {
    let outcome = run(HookRunRequest {
        root: root.to_path_buf(),
        stages: vec![stage_of(stage, hook)],
        git_args: git_args.iter().map(|arg| (*arg).to_string()).collect(),
        message_file: git_args.first().map(Into::into),
        ..HookRunRequest::default()
    })
    .expect("run");
    assert!(outcome.success(), "{outcome:?}");
    std::fs::read_to_string(root.join("out.txt")).unwrap_or_default()
}

fn job(command: &str) -> Hook {
    let mut hook = Hook::run("job", command);
    hook.always_run = true;
    hook
}

#[test]
fn a_post_checkout_job_receives_git_arguments_as_positionals() {
    let dir = TempDir::new().unwrap();
    let out = run_with_args(
        dir.path(),
        Stage::PostCheckout,
        job("printf '%s|' \"$@\" > out.txt"),
        &["A", "B space", "1"],
    );
    assert_eq!(out, "A|B space|1|");
}

#[test]
fn a_prepare_commit_msg_job_sees_the_source_and_commit_object() {
    let dir = TempDir::new().unwrap();
    let out = run_with_args(
        dir.path(),
        Stage::PrepareCommitMsg,
        job("printf '%s|' \"$@\" > out.txt"),
        &[".git/COMMIT_EDITMSG", "commit", "abc123"],
    );
    assert_eq!(out, ".git/COMMIT_EDITMSG|commit|abc123|");
}

#[test]
fn a_message_stage_without_git_arguments_still_gets_the_message_file() {
    let dir = TempDir::new().unwrap();
    let outcome = run(HookRunRequest {
        root: dir.path().to_path_buf(),
        stages: vec![stage_of(Stage::CommitMsg, job("printf '%s|' \"$@\" > out.txt"))],
        message_file: Some("MSG".into()),
        ..HookRunRequest::default()
    })
    .expect("run");
    assert!(outcome.success());
    assert_eq!(std::fs::read_to_string(dir.path().join("out.txt")).unwrap(), "MSG|");
}

#[test]
fn git_arguments_are_exported_to_the_environment() {
    let dir = TempDir::new().unwrap();
    let mut hook = job("printf '%s %s %s' \"$POLY_GIT_ARG_COUNT\" \"$POLY_GIT_ARG_1\" \"$POLY_GIT_ARG_3\" > out.txt");
    hook.pass_filenames = false;
    let out = run_with_args(dir.path(), Stage::PostMerge, hook, &["x", "y", "z"]);
    assert_eq!(out, "3 x z");
}

#[test]
fn a_job_that_takes_no_filenames_gets_no_positionals() {
    let dir = TempDir::new().unwrap();
    let mut hook = job("printf '%s' \"$#\" > out.txt");
    hook.pass_filenames = false;
    assert_eq!(
        run_with_args(dir.path(), Stage::PostCheckout, hook, &["A", "B", "1"]),
        "0"
    );
}
