//! How a priority group is scheduled: what runs beside what.
//!
//! Every assertion here is about **observed overlap**, never about elapsed
//! time. Two hooks that must overlap rendezvous through the filesystem — each
//! waits for the other's marker — so a scheduler that serializes them makes the
//! wait time out and the test fail; a loaded machine only makes the rendezvous
//! slower, never wrong. Two hooks that must *not* overlap append `start` /
//! `end` markers to a shared log, and the assertion is that the log never
//! interleaves them.

#![cfg(unix)]

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use poly_hooks::model::{CARGO_SERIAL_GROUP, HookStatus};
use poly_hooks::timeout::HookTimeout;
use poly_hooks::{Hook, HookRunRequest, Stage, StageSpec, run};
use tempfile::TempDir;

/// How many 10ms polls a rendezvous waits before giving up. Generous (20s) so a
/// loaded machine never trips it, bounded so a scheduling regression fails the
/// test instead of hanging it.
const RENDEZVOUS_POLLS: usize = 2000;

fn init_repo() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path();
    for args in [
        ["init", "-q"].as_slice(),
        ["config", "user.email", "test@example.com"].as_slice(),
        ["config", "user.name", "Test"].as_slice(),
    ] {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("git invocation");
        assert!(output.status.success(), "git {args:?} failed");
    }
    dir
}

fn cmd_hook(id: &str, command: &str) -> Hook {
    let mut hook = Hook::run(id, command);
    hook.always_run = true;
    hook.pass_filenames = false;
    hook
}

/// A shell fragment that blocks until `marker` exists, and exits non-zero if it
/// never does — so "these never overlapped" surfaces as a hook failure rather
/// than as a hung test.
///
/// The trailing `true` keeps the fragment safe to end a command with: the runner
/// appends `"$@"`, and a `done "$@"` is a syntax error.
fn wait_for(marker: &str) -> String {
    format!(
        "i=0; while [ ! -f {marker} ]; do i=$((i+1)); \
         if [ $i -gt {RENDEZVOUS_POLLS} ]; then echo \"never saw {marker}\" >&2; exit 1; fi; sleep 0.01; done; true"
    )
}

/// A hook that announces itself and then waits for its peer: it can only pass if
/// the two ran at the same time.
fn rendezvous_hook(id: &str, peer: &str) -> Hook {
    cmd_hook(id, &format!("touch {id}.here; {}", wait_for(&format!("{peer}.here"))))
}

/// A hook that brackets a short body with `start` / `end` markers in a shared
/// log, so an overlap with a set peer is visible as interleaving.
fn logging_hook(id: &str, group: &str) -> Hook {
    let mut hook = cmd_hook(
        id,
        &format!("printf '{id}-start\\n' >> log.txt; sleep 0.2; printf '{id}-end\\n' >> log.txt"),
    );
    hook.serial_group = Some(group.to_string());
    hook
}

fn run_stage(root: &Path, hooks: Vec<Hook>) -> poly_hooks::HookRunOutcome {
    let request = HookRunRequest {
        root: root.to_path_buf(),
        stages: vec![StageSpec {
            stage: Stage::PreCommit,
            hooks,
            ..StageSpec::default()
        }],
        // Pinned so the schedule under test is the runner's, not the host's
        // core count — and so a single-core CI box cannot serialize the
        // rendezvous by starving the pool.
        concurrency: Some(4),
        ..HookRunRequest::default()
    };
    run(request).expect("run")
}

fn log_lines(root: &Path) -> Vec<String> {
    std::fs::read_to_string(root.join("log.txt"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Every `<id>-start` is immediately followed by its own `<id>-end`: no member
/// of the set was running while another one was.
fn assert_no_overlap(lines: &[String]) {
    assert!(!lines.is_empty(), "the set members did not run at all");
    for pair in lines.chunks(2) {
        assert_eq!(pair.len(), 2, "a hook logged a start without an end: {lines:?}");
        let id = pair[0].strip_suffix("-start").unwrap_or_else(|| {
            panic!("expected a start marker, got {:?} in {lines:?}", pair[0]);
        });
        assert_eq!(pair[1], format!("{id}-end"), "set members overlapped: {lines:?}");
    }
}

/// The baseline the whole model rests on: hooks in one priority group run at the
/// same time.
#[test]
fn hooks_in_a_priority_group_run_concurrently() {
    let repo = init_repo();
    let outcome = run_stage(repo.path(), vec![rendezvous_hook("a", "b"), rendezvous_hook("b", "a")]);
    assert!(
        outcome.success(),
        "hooks did not overlap: {:?}",
        outcome.stages[0].hooks
    );
}

/// A hook that must not run beside a peer no longer drags the whole group down
/// with it: this is the regression that made a repo's *entire* pre-commit stage
/// serial as soon as one inline job carried the schema-default
/// `parallel = false`.
#[test]
fn a_serial_hook_does_not_serialize_the_rest_of_its_group() {
    let repo = init_repo();
    let mut alone = cmd_hook("alone", "printf x > alone.out");
    alone.require_serial = true;

    let outcome = run_stage(
        repo.path(),
        vec![alone, rendezvous_hook("a", "b"), rendezvous_hook("b", "a")],
    );

    assert!(
        outcome.success(),
        "a serial peer must not serialize the group: {:?}",
        outcome.stages[0].hooks
    );
    assert_eq!(
        std::fs::read_to_string(repo.path().join("alone.out")).unwrap_or_default(),
        "x",
        "the serial hook must still run"
    );
}

/// The exclusion set is mutual exclusion, not a stop-the-world: two members
/// never overlap each other, while a hook outside the set overlaps them freely.
///
/// The non-member proves its own overlap: each member waits for the
/// non-member's marker before finishing, so if the non-member had been queued
/// behind the set, the members would fail rather than pass.
#[test]
fn set_members_never_overlap_while_a_non_member_runs_alongside() {
    let repo = init_repo();
    let root = repo.path();

    let mut first = logging_hook("a", "res");
    let mut second = logging_hook("b", "res");
    for member in [&mut first, &mut second] {
        let poll = wait_for("outsider.here");
        let id = member.id.clone();
        member.command = poly_hooks::model::HookCommand::Run(format!(
            "printf '{id}-start\\n' >> log.txt; {poll}; printf '{id}-end\\n' >> log.txt"
        ));
    }
    let outsider = cmd_hook("outsider", &format!("{}; touch outsider.here", wait_for("log.txt")));

    let outcome = run_stage(root, vec![first, second, outsider]);

    assert!(
        outcome.success(),
        "the non-member must run alongside the set: {:?}",
        outcome.stages[0].hooks
    );
    assert_no_overlap(&log_lines(root));
}

/// The cargo set is just an exclusion set with a well-known name, and it holds
/// for any hook that joins it.
#[test]
fn cargo_set_members_never_overlap() {
    let repo = init_repo();
    let root = repo.path();

    let outcome = run_stage(
        root,
        vec![
            logging_hook("clippy", CARGO_SERIAL_GROUP),
            logging_hook("deny", CARGO_SERIAL_GROUP),
            logging_hook("sort", CARGO_SERIAL_GROUP),
        ],
    );

    assert!(outcome.success());
    let lines = log_lines(root);
    assert_eq!(lines.len(), 6, "every cargo hook must run: {lines:?}");
    assert_no_overlap(&lines);
}

/// Two *different* sets do not exclude each other — only same-named members do.
#[test]
fn hooks_in_different_sets_run_concurrently() {
    let repo = init_repo();

    let mut first = rendezvous_hook("a", "b");
    first.serial_group = Some("cargo".to_string());
    let mut second = rendezvous_hook("b", "a");
    second.serial_group = Some("npm".to_string());

    let outcome = run_stage(repo.path(), vec![first, second]);
    assert!(
        outcome.success(),
        "different sets must not exclude each other: {:?}",
        outcome.stages[0].hooks
    );
}

/// A hook queued behind a set peer is **not running**, so it is not charged for
/// the peer's time: its budget starts when it is spawned.
///
/// This is the false kill the exclusion set exists to prevent — a `cargo deny
/// check` that takes seconds, reported as timed out after half an hour of
/// somebody else's cold build.
#[test]
fn a_queued_hook_is_not_charged_for_its_predecessor() {
    let repo = init_repo();

    let mut slow = cmd_hook("slow", "sleep 3");
    slow.serial_group = Some("res".to_string());
    let mut quick = cmd_hook("quick", "printf x > quick.out");
    quick.serial_group = Some("res".to_string());
    // Far below the predecessor's runtime: only a clock that starts at spawn
    // can leave this hook passing.
    quick.timeout = HookTimeout::Limit(Duration::from_secs(2));

    let outcome = run_stage(repo.path(), vec![slow, quick]);

    let quick = outcome.stages[0]
        .hooks
        .iter()
        .find(|hook| hook.id == "quick")
        .expect("the queued hook is reported");
    assert_eq!(
        quick.status,
        HookStatus::Passed,
        "a queued hook must not be killed for its predecessor's time"
    );
    assert!(outcome.success());
}

/// Ordering guarantees survive the chain scheduling: a set's members run in
/// config order, and the report stays in position order.
#[test]
fn set_members_run_in_config_order_and_report_in_position_order() {
    let repo = init_repo();
    let root = repo.path();

    let hooks = ["one", "two", "three"]
        .iter()
        .map(|id| {
            let mut hook = cmd_hook(id, &format!("printf '{id} ' >> order.txt"));
            hook.serial_group = Some("res".to_string());
            hook
        })
        .collect();

    let outcome = run_stage(root, hooks);

    assert!(outcome.success());
    assert_eq!(
        std::fs::read_to_string(root.join("order.txt")).unwrap_or_default(),
        "one two three ",
        "a set runs its members in config order"
    );
    let ids: Vec<&str> = outcome.stages[0].hooks.iter().map(|hook| hook.id.as_str()).collect();
    assert_eq!(ids, vec!["one", "two", "three"]);
    let positions: Vec<usize> = outcome.stages[0].hooks.iter().map(|hook| hook.position).collect();
    assert_eq!(positions, vec![0, 1, 2]);
}
