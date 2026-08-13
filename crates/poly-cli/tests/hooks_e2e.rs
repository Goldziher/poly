//! End-to-end coverage for `poly hooks` driving the native runner in a real
//! temporary git repository.
//!
//! These tests shell out to the built `poly` binary (via `CARGO_BIN_EXE_poly`)
//! and exercise the full path: config discovery → lowering → `poly_hooks::run`
//! → reporter → process exit code, plus `install` / `hook-impl`.
//!
//! The job command lines use POSIX shell syntax (`printf`, `true`, `false`),
//! which the runner feeds to `sh -c`; they would not run under `cmd /C`, so the
//! whole suite is gated to Unix. A Windows equivalent would need `cmd`-syntax
//! commands.
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

const POLY: &str = env!("CARGO_BIN_EXE_poly");

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git invocation");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn init_repo() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path();
    git(path, &["init", "-q"]);
    git(path, &["config", "user.email", "test@example.com"]);
    git(path, &["config", "user.name", "Test"]);
    git(path, &["config", "commit.gpgsign", "false"]);
    dir
}

fn write(repo: &Path, name: &str, contents: &str) {
    std::fs::write(repo.join(name), contents).expect("write file");
}

fn staged_blob(repo: &Path, name: &str) -> String {
    git(repo, &["show", &format!(":{name}")])
}

fn poly_hooks(repo: &Path, args: &[&str]) -> Output {
    Command::new(POLY)
        .arg("hooks")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("poly invocation")
}

/// A config with a parallel pre-commit stage: a no-op job plus a `stage_fixed`
/// job that rewrites a staged file.
fn stage_fixed_config(stage_fixed: bool) -> String {
    format!(
        r#"
[hooks.pre-commit]
parallel = true

[[hooks.pre-commit.jobs]]
name = "noop"
run = "true"

[[hooks.pre-commit.jobs]]
name = "fixer"
run = "printf changed > fixed.txt"
stage_fixed = {stage_fixed}
"#
    )
}

#[test]
fn run_pre_commit_runs_all_hooks_and_restages_stage_fixed_change() {
    let repo = init_repo();
    let root = repo.path();
    write(root, "poly.toml", &stage_fixed_config(true));
    write(root, "fixed.txt", "orig");
    git(root, &["add", "fixed.txt"]);

    let output = poly_hooks(root, &["run", "pre-commit"]);
    let report = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(report.contains("noop"), "missing noop:\n{report}");
    assert!(report.contains("fixer"), "missing fixer:\n{report}");
    let noop_at = report.find("noop").unwrap();
    let fixer_at = report.find("fixer").unwrap();
    assert!(noop_at < fixer_at, "hooks not index-ordered:\n{report}");
    assert_eq!(staged_blob(root, "fixed.txt"), "changed");
}

#[test]
fn stage_fixed_false_leaves_modification_unstaged() {
    let repo = init_repo();
    let root = repo.path();
    write(root, "poly.toml", &stage_fixed_config(false));
    write(root, "fixed.txt", "orig");
    git(root, &["add", "fixed.txt"]);

    let output = poly_hooks(root, &["run", "pre-commit"]);
    assert!(output.status.success());
    assert_eq!(staged_blob(root, "fixed.txt"), "orig");
    assert_eq!(std::fs::read_to_string(root.join("fixed.txt")).unwrap(), "changed");
}

#[test]
fn run_with_single_job_forces_serial_and_passes() {
    let repo = init_repo();
    let root = repo.path();
    write(root, "poly.toml", &stage_fixed_config(true));
    write(root, "fixed.txt", "orig");
    git(root, &["add", "fixed.txt"]);

    let output = poly_hooks(root, &["run", "pre-commit", "-j", "1"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(staged_blob(root, "fixed.txt"), "changed");
}

#[test]
fn failing_job_yields_non_zero_exit() {
    let repo = init_repo();
    let root = repo.path();
    write(
        root,
        "poly.toml",
        r#"
[hooks.pre-commit]
[[hooks.pre-commit.jobs]]
name = "boom"
run = "false"
"#,
    );

    let output = poly_hooks(root, &["run", "pre-commit"]);
    assert!(!output.status.success(), "a failing job must produce a non-zero exit");
}

#[test]
fn hook_impl_pre_commit_runs_and_restages() {
    let repo = init_repo();
    let root = repo.path();
    write(root, "poly.toml", &stage_fixed_config(true));
    write(root, "fixed.txt", "orig");
    git(root, &["add", "fixed.txt"]);

    let output = poly_hooks(root, &["hook-impl", "--hook-type=pre-commit", "--"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = String::from_utf8_lossy(&output.stdout);
    assert!(report.contains("noop") && report.contains("fixer"), "{report}");
    assert_eq!(staged_blob(root, "fixed.txt"), "changed");
}

#[test]
fn hook_impl_commit_msg_enforces_conventional_commits() {
    let repo = init_repo();
    let root = repo.path();
    write(root, "poly.toml", "[hooks.builtin]\ncommit = true\n");

    write(root, "msg-bad.txt", "nope: not a conventional type\n");
    let bad = poly_hooks(root, &["hook-impl", "--hook-type=commit-msg", "--", "msg-bad.txt"]);
    let bad_report = String::from_utf8_lossy(&bad.stdout);
    assert!(!bad.status.success(), "bad message should fail: {bad_report}");
    assert!(
        bad_report.contains("poly-commit"),
        "poly-commit must run, got: {bad_report}"
    );

    write(root, "msg-good.txt", "feat: add a thing\n");
    let good = poly_hooks(root, &["hook-impl", "--hook-type=commit-msg", "--", "msg-good.txt"]);
    assert!(
        good.status.success(),
        "good message should pass: {}",
        String::from_utf8_lossy(&good.stdout)
    );
}

#[test]
fn run_pre_commit_caches_second_unchanged_run_and_no_cache_forces_rerun() {
    let repo = init_repo();
    let root = repo.path();
    // The sentinel is written outside the repository on purpose: a `pre-commit`
    // run is scoped to staged content, so a hook's working directory is the
    // staged snapshot rather than the worktree. Counting executions through an
    // absolute path keeps this test about caching and nothing else.
    let log = tempfile::tempdir().expect("sentinel dir");
    let log_path = log.path().join("runs.log");
    write(
        root,
        "poly.toml",
        &format!(
            r#"
[hooks.pre-commit]
[[hooks.pre-commit.jobs]]
name = "sentinel"
run = "printf x >> {}"
cache = {{ inputs = ["tracked.txt"] }}
"#,
            log_path.display()
        ),
    );
    write(root, "tracked.txt", "content");
    git(root, &["add", "tracked.txt"]);
    git(root, &["commit", "-qm", "init"]);

    let runs = || std::fs::read_to_string(&log_path).unwrap_or_default();

    let first = poly_hooks(root, &["run", "pre-commit"]);
    assert!(
        first.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert_eq!(runs(), "x");

    let second = poly_hooks(root, &["run", "pre-commit"]);
    assert!(second.status.success());
    let report = String::from_utf8_lossy(&second.stdout);
    assert!(report.contains("(cached)"), "second run not cached:\n{report}");
    assert_eq!(runs(), "x", "cached hook must not re-execute");

    let third = poly_hooks(root, &["run", "pre-commit", "--no-cache"]);
    assert!(third.status.success());
    assert_eq!(runs(), "xx", "--no-cache must re-execute");
}

#[test]
fn install_writes_a_shim_that_git_commit_triggers() {
    let repo = init_repo();
    let root = repo.path();
    // Outside the repository on purpose: a real `git commit` gates on staged
    // content, so the hook's working directory is the staged snapshot. An
    // absolute sentinel proves the shim reached the runner without depending on
    // which tree the hook ran in.
    let sentinel_dir = tempfile::tempdir().expect("sentinel dir");
    let sentinel = sentinel_dir.path().join("sentinel.created");
    write(
        root,
        "poly.toml",
        &format!(
            r#"
[hooks.pre-commit]
[[hooks.pre-commit.jobs]]
name = "sentinel"
run = "touch {}"
"#,
            sentinel.display()
        ),
    );

    let installed = poly_hooks(root, &["install", "--hook-type", "pre-commit"]);
    assert!(
        installed.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&installed.stderr)
    );
    let shim = root.join(".git/hooks/pre-commit");
    assert!(shim.is_file(), "shim was not written");
    assert!(
        std::fs::read_to_string(&shim)
            .unwrap()
            .contains("hook-impl --hook-type=pre-commit"),
        "shim missing exec line"
    );

    write(root, "tracked.txt", "content");
    git(root, &["add", "tracked.txt"]);
    let poly_dir = Path::new(POLY).parent().expect("poly binary has a parent dir");
    let augmented_path = match std::env::var_os("PATH") {
        Some(existing) => format!("{}:{}", poly_dir.display(), existing.to_string_lossy()),
        None => poly_dir.display().to_string(),
    };
    let commit = Command::new("git")
        .args(["commit", "-q", "-m", "feat: trigger hook"])
        .current_dir(root)
        .env("PATH", augmented_path)
        .output()
        .expect("git commit");
    assert!(
        commit.status.success(),
        "commit (with hook) failed: {}",
        String::from_utf8_lossy(&commit.stderr)
    );

    assert!(
        sentinel.exists(),
        "installed pre-commit shim did not trigger the native runner"
    );
}

/// A cargo job held back before it is spawned must **say so**, naming the lock
/// and saying that its budget has not started.
///
/// The state it must not be confused with is "running": a hook waiting on
/// cargo's package-cache lock is silent, and silence that looks like work is
/// exactly the report this subsystem exists to prevent. The notice goes to
/// stderr, so this runs the real binary and reads the real stream rather than
/// asserting on a formatter in isolation.
#[test]
fn a_cargo_job_held_back_by_an_external_lock_reports_the_wait() {
    use std::io::Write as _;
    use std::os::fd::AsRawFd as _;

    /// Longer than the pre-spawn announce threshold (2s), so exactly one notice
    /// is due while the lock is held.
    const HOLD: std::time::Duration = std::time::Duration::from_secs(3);

    let repo = init_repo();
    let root = repo.path();
    write(
        root,
        "poly.toml",
        r#"
[hooks.pre-commit]
parallel = true

[[hooks.pre-commit.jobs]]
name = "cargo-deny"
run = "true"
serial = "cargo"
workspace = true
"#,
    );
    write(root, "tracked.txt", "content");
    git(root, &["add", "tracked.txt"]);

    let cargo_home = TempDir::new().expect("cargo home");
    // Load the binary before the clock starts. A cold `poly` — hundreds of
    // megabytes unoptimised — can take seconds to map in, and that would burn
    // the hold before the run ever reaches the probe, leaving a green test that
    // proved nothing. It runs against the same empty `CARGO_HOME`, so the test
    // never probes (or waits on) the developer's real package cache.
    let warm = Command::new(POLY)
        .args(["hooks", "run", "pre-commit"])
        .current_dir(root)
        .env("CARGO_HOME", cargo_home.path())
        .output()
        .expect("poly invocation");
    assert!(warm.status.success(), "the warm-up run must pass with a free lock");

    let lock = cargo_home.path().join(".package-cache");
    let (ready, holding) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let mut file = std::fs::File::create(&lock).expect("create the package-cache lock");
        file.write_all(b"held").expect("write the package-cache lock");
        // SAFETY: the descriptor is owned by `file`, which outlives the call.
        let taken = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        assert_eq!(taken, 0, "the holder must actually acquire the lock");
        ready.send(()).expect("signal that the lock is held");
        std::thread::sleep(HOLD);
        // SAFETY: as above.
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    });
    // The run must not start before the lock is genuinely held, or it probes a
    // free lock and the test proves nothing.
    holding.recv().expect("wait until the lock is held");

    let output = Command::new(POLY)
        .args(["hooks", "run", "pre-commit"])
        .current_dir(root)
        .env("CARGO_HOME", cargo_home.path())
        .output()
        .expect("poly invocation");
    holder.join().expect("the holder thread");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "the job must still run, late: {stderr}\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        stderr.contains("⏸ waiting to start: cargo-deny"),
        "a held-back job must name itself and its state:\n{stderr}"
    );
    assert!(
        stderr.contains("cargo's package cache lock is held by a process outside this run"),
        "the notice must name what it is waiting on:\n{stderr}"
    );
    assert!(
        stderr.contains("its time budget has not started"),
        "a job that has not been spawned has no clock running:\n{stderr}"
    );
    assert!(
        !stderr.contains("still running: cargo-deny"),
        "a job that has not been spawned must not be reported as running:\n{stderr}"
    );
}
