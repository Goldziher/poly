//! A hook running in the staged snapshot still sees the original repository.
//!
//! The snapshot is a plain directory in the per-user cache, not a git checkout,
//! so a hook that enumerates the index (`git ls-files` feeding a whole-tree
//! secret scan) needs the run to hand it the repository's absolute `GIT_DIR`
//! and the index the in-flight commit is building (Goldziher/poly#37). These
//! tests pin that context down, and pin the two ways it must *not* reach back
//! into the original repository: an opportunistic index refresh and a
//! snapshot-local `git init`.
//!
//! The partial-commit test sets `GIT_INDEX_FILE` in the process environment, the
//! way git does for `git commit <paths>`. The environment is global, so every
//! test here holds [`ENV_LOCK`] — the others read it when the runner resolves
//! the index.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard, PoisonError};

use poly_hooks::{Hook, HookRunRequest, Stage, StageSpec, run};
use tempfile::TempDir;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn env_lock() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `GIT_INDEX_FILE` set for the guard's lifetime, removed again on drop (panic
/// included) while [`ENV_LOCK`] is still held.
struct IndexFileEnv {
    _lock: MutexGuard<'static, ()>,
}

impl IndexFileEnv {
    fn set(value: &str) -> Self {
        let lock = env_lock();
        // SAFETY: every environment access in this binary that can race this
        // write happens while holding `ENV_LOCK`, which this guard holds until
        // the variable is removed again.
        unsafe { std::env::set_var("GIT_INDEX_FILE", value) };
        Self { _lock: lock }
    }
}

impl Drop for IndexFileEnv {
    fn drop(&mut self) {
        // SAFETY: as in `set` — `ENV_LOCK` is still held.
        unsafe { std::env::remove_var("GIT_INDEX_FILE") };
    }
}

fn git_in(repo: &Path, args: &[&str], index: Option<&Path>) -> String {
    let mut command = Command::new("git");
    command.args(args).current_dir(repo);
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    }
    let output = command.output().expect("git invocation");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn git(repo: &Path, args: &[&str]) -> String {
    git_in(repo, args, None)
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

fn write(root: &Path, name: &str, content: &str) {
    std::fs::write(root.join(name), content).expect("write file");
}

/// Materialize `index` (the default index when `None`) into `dest`, as the real
/// snapshot does, without touching the per-user cache.
fn snapshot_of(repo: &Path, dest: &Path, index: Option<&Path>) {
    let prefix = format!("--prefix={}/", dest.display());
    git_in(repo, &["checkout-index", "-f", "-a", &prefix], index);
}

/// A hook that takes no file list and always runs, writing what it saw to `out`.
fn hook(command: &str) -> Hook {
    let mut hook = Hook::run("probe", command);
    hook.always_run = true;
    hook.pass_filenames = false;
    hook
}

fn run_isolated(root: &Path, snapshot: &Path, hook: Hook) {
    let outcome = run(HookRunRequest {
        root: root.to_path_buf(),
        work_root: Some(snapshot.to_path_buf()),
        stages: vec![StageSpec {
            stage: Stage::PreCommit,
            hooks: vec![hook],
            ..StageSpec::default()
        }],
        ..HookRunRequest::default()
    })
    .expect("run");
    assert!(outcome.success(), "the isolated hook failed: {outcome:?}");
}

fn ls_files_hook(out: &Path) -> Hook {
    hook(&format!("git ls-files > '{}'", out.display()))
}

fn lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .expect("hook output")
        .lines()
        .map(str::to_string)
        .collect()
}

fn scratch_file(dir: &TempDir, name: &str) -> PathBuf {
    dir.path().join(name)
}

/// The reporter's case: a whole-tree scan enumerates the index from inside the
/// snapshot. It must see every staged entry — including one deleted from the
/// worktree after staging — and nothing untracked.
#[test]
fn an_isolated_hook_enumerates_the_original_index() {
    let _env = env_lock();
    let repo = init_repo();
    let root = repo.path();
    write(root, "kept.txt", "kept\n");
    git(root, &["add", "kept.txt"]);
    git(root, &["commit", "-qm", "init"]);
    write(root, "staged.txt", "staged\n");
    git(root, &["add", "staged.txt"]);
    std::fs::remove_file(root.join("staged.txt")).unwrap();
    write(root, "untracked.txt", "not in the index\n");

    let snapshot = TempDir::new().unwrap();
    snapshot_of(root, snapshot.path(), None);
    let scratch = TempDir::new().unwrap();
    let out = scratch_file(&scratch, "ls-files.txt");

    run_isolated(root, snapshot.path(), ls_files_hook(&out));

    assert_eq!(lines(&out), ["kept.txt", "staged.txt"]);
}

/// `git commit <paths>` (and `-a`) builds the commit in an alternate index and
/// exports it as `GIT_INDEX_FILE`; a normal commit exports the relative
/// `.git/index`. The hook must enumerate the index the commit is building,
/// resolved against the repository rather than the snapshot it runs in.
#[test]
fn an_isolated_hook_enumerates_the_inherited_alternate_index() {
    let repo = init_repo();
    let root = repo.path();
    write(root, "kept.txt", "kept\n");
    git(root, &["add", "kept.txt"]);
    git(root, &["commit", "-qm", "init"]);
    // Staged in the default index only: not part of this partial commit.
    write(root, "elsewhere.txt", "staged in the default index\n");
    git(root, &["add", "elsewhere.txt"]);
    // The partial commit's own index: HEAD plus `partial.txt`.
    let alternate = root.join(".git").join("next-index-test.lock");
    write(root, "partial.txt", "partial\n");
    git_in(root, &["read-tree", "HEAD"], Some(&alternate));
    git_in(root, &["add", "partial.txt"], Some(&alternate));

    let snapshot = TempDir::new().unwrap();
    snapshot_of(root, snapshot.path(), Some(&alternate));
    let scratch = TempDir::new().unwrap();
    let out = scratch_file(&scratch, "ls-files.txt");

    // Relative, as git exports `.git/index` for an ordinary commit: it names a
    // file under the repository, not under the snapshot the hook runs in.
    let _env = IndexFileEnv::set(".git/next-index-test.lock");
    run_isolated(root, snapshot.path(), ls_files_hook(&out));

    assert_eq!(lines(&out), ["kept.txt", "partial.txt"]);
}

/// In a linked worktree the git dir is `.git/worktrees/<name>` and the index is
/// that worktree's own; assuming `<root>/.git` would find a file, not a dir.
#[test]
fn an_isolated_hook_in_a_linked_worktree_sees_that_worktrees_index() {
    let _env = env_lock();
    let repo = init_repo();
    let main = repo.path();
    write(main, "kept.txt", "kept\n");
    git(main, &["add", "kept.txt"]);
    git(main, &["commit", "-qm", "init"]);
    let holder = TempDir::new().unwrap();
    let linked = holder.path().join("linked");
    git(main, &["worktree", "add", "-q", linked.to_str().unwrap()]);
    write(&linked, "linked-only.txt", "linked\n");
    git(&linked, &["add", "linked-only.txt"]);

    let snapshot = TempDir::new().unwrap();
    snapshot_of(&linked, snapshot.path(), None);
    let scratch = TempDir::new().unwrap();
    let out = scratch_file(&scratch, "ls-files.txt");

    run_isolated(&linked, snapshot.path(), ls_files_hook(&out));

    assert_eq!(lines(&out), ["kept.txt", "linked-only.txt"]);
}

/// `git status` refreshes the index's stat cache as a side effect. Run against
/// the snapshot, that refresh would write the snapshot's stat data into the
/// commit's index; the context must suppress it.
#[test]
fn git_status_in_an_isolated_hook_leaves_the_original_index_untouched() {
    let _env = env_lock();
    let repo = init_repo();
    let root = repo.path();
    write(root, "kept.txt", "kept\n");
    git(root, &["add", "kept.txt"]);
    git(root, &["commit", "-qm", "init"]);
    let index = root.join(".git").join("index");
    let before = std::fs::read(&index).unwrap();

    let snapshot = TempDir::new().unwrap();
    snapshot_of(root, snapshot.path(), None);
    run_isolated(root, snapshot.path(), hook("git status --short >/dev/null"));

    assert!(
        std::fs::read(&index).unwrap() == before,
        "a hook's `git status` rewrote the original repository's index"
    );
}

/// A snapshot-local `git init` resolves to the exported `GIT_DIR`. That is a
/// harmless re-initialization as long as no `GIT_WORK_TREE` is exported —
/// with one, git records the snapshot as the repository's `core.worktree`,
/// silently re-pointing the real checkout at a cache directory.
#[test]
fn git_init_in_an_isolated_hook_does_not_repoint_the_original_worktree() {
    let _env = env_lock();
    let repo = init_repo();
    let root = repo.path();
    write(root, "kept.txt", "kept\n");
    git(root, &["add", "kept.txt"]);
    git(root, &["commit", "-qm", "init"]);
    let config = root.join(".git").join("config");
    let before = std::fs::read_to_string(&config).unwrap();

    let snapshot = TempDir::new().unwrap();
    snapshot_of(root, snapshot.path(), None);
    run_isolated(root, snapshot.path(), hook("git init -q >/dev/null 2>&1; true"));

    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        before,
        "a hook's `git init` rewrote the original repository's config"
    );
}
