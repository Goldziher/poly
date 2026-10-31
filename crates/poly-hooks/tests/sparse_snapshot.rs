//! A sparse staged snapshot holds the staged files and their directory context,
//! not the whole tree, and every byte in it still comes from the index.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

use poly_hooks::filter::FilePattern;
use poly_hooks::snapshot::{SnapshotScope, StagedSnapshot, referenced_paths};
use poly_hooks::{Hook, HookRunRequest, Stage, StageSpec};
use tempfile::TempDir;

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git").args(args).current_dir(repo).output().expect("git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn write(repo: &Path, name: &str, content: &str) {
    let path = repo.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn repo() -> TempDir {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "t@example.com"]);
    git(root, &["config", "user.name", "T"]);
    git(root, &["config", "commit.gpgsign", "false"]);
    write(root, "pyproject.toml", "[tool.ruff]\n");
    write(root, "pkg/__init__.py", "");
    write(root, "pkg/a.py", "a = 1\n");
    write(root, "pkg/sub/b.py", "b = 1\n");
    write(root, "other/c.py", "c = 1\n");
    write(root, "other/deep/d.py", "d = 1\n");
    write(root, "scripts/check.sh", "echo ok\n");
    write(root, "scripts/lib.sh", "echo lib\n");
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "init"]);
    dir
}

/// `rel` exists in `snap` as a real file or directory: no component on the way is
/// a link to the worktree.
fn is_written(snap: &Path, rel: &str) -> bool {
    let mut current = snap.to_path_buf();
    for component in Path::new(rel).components() {
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => return false,
            Ok(_) => {}
            Err(_) => return false,
        }
    }
    true
}

/// The inode of `path`, read without following a symlink.
fn inode(path: &Path) -> u64 {
    std::os::unix::fs::MetadataExt::ino(&std::fs::symlink_metadata(path).unwrap())
}

/// `rel` is a regular file in `snap` that shares an inode with the same path in
/// the worktree — i.e. a hardlink the mirror made, not a byte copy.
fn is_hardlinked(snap: &Path, repo: &Path, rel: &str) -> bool {
    let mirrored = snap.join(rel);
    let live = repo.join(rel);
    let Ok(meta) = std::fs::symlink_metadata(&mirrored) else {
        return false;
    };
    meta.file_type().is_file() && is_written(snap, rel) && inode(&mirrored) == inode(&live)
}

/// Every regular file reachable from `root` by a walk that **never follows a
/// directory symlink** — the walker `ai-rulez validate` and friends use.
fn walked_files(root: &Path) -> Vec<PathBuf> {
    fn visit(dir: &Path, root: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if file_type.is_dir() {
                visit(&path, root, out);
            } else if file_type.is_file() {
                out.push(path.strip_prefix(root).unwrap().to_path_buf());
            }
        }
    }
    let mut out = Vec::new();
    visit(root, root, &mut out);
    out.sort();
    out
}

fn sparse<'a>(files: &'a [PathBuf], pinned: &'a [String]) -> SnapshotScope<'a> {
    SnapshotScope::Sparse { files, pinned }
}

fn snapshot(repo: &Path, cache: &Path, files: &[PathBuf], pinned: &[String]) -> StagedSnapshot {
    StagedSnapshot::create_scoped_in(cache, repo, &[], sparse(files, pinned)).unwrap()
}

#[test]
fn sparse_snapshot_writes_staged_files_and_directory_context_only() {
    let repo = repo();
    let cache = TempDir::new().unwrap();
    let files = [PathBuf::from("pkg/sub/b.py")];

    let snap = snapshot(repo.path(), cache.path(), &files, &[]);

    for present in ["pkg/sub/b.py", "pkg/__init__.py", "pkg/a.py", "pyproject.toml"] {
        assert!(is_written(snap.path(), present), "{present} should be materialized");
    }
    // Everything else is mirrored, not written from the index: present to a
    // walker, but the same inode as the worktree rather than a fresh copy.
    for mirrored in ["other/c.py", "other/deep/d.py", "scripts/check.sh", "scripts/lib.sh"] {
        assert!(
            is_hardlinked(snap.path(), repo.path(), mirrored),
            "{mirrored} should be mirrored from the worktree"
        );
    }
}

#[test]
fn unselected_directories_are_real_trees_so_a_non_following_walk_sees_them() {
    let repo = repo();
    let cache = TempDir::new().unwrap();
    let files = [PathBuf::from("pkg/sub/b.py")];

    let snap = snapshot(repo.path(), cache.path(), &files, &[]);

    let other = snap.path().join("other");
    assert!(
        !std::fs::symlink_metadata(&other).unwrap().file_type().is_symlink(),
        "a directory symlink is invisible to a walker that does not follow one"
    );
    assert!(other.is_dir());
    assert!(
        snap.path().join("other/deep").is_dir(),
        "nested directories are real too"
    );
    assert!(snap.path().join("other/c.py").is_file(), "so is a module file");
    assert!(is_hardlinked(snap.path(), repo.path(), "other/c.py"));
}

/// THE AR101 REGRESSION (Goldziher/poly#45). A tool that walks the snapshot
/// without following directory symlinks — `ai-rulez validate` resolving rule
/// globs is the one that reported it — must reach every tracked file, including
/// those under a directory the sparse selection did not write. A directory
/// symlink hid all of them.
#[test]
fn a_walk_that_does_not_follow_directory_symlinks_sees_every_tracked_file() {
    let repo = repo();
    let cache = TempDir::new().unwrap();
    let files = [PathBuf::from("pkg/sub/b.py")];

    let snap = snapshot(repo.path(), cache.path(), &files, &[]);

    let walked = walked_files(snap.path());
    for tracked in [
        "pkg/a.py",
        "pkg/sub/b.py",
        "other/c.py",
        "other/deep/d.py",
        "scripts/check.sh",
        "scripts/lib.sh",
        "pyproject.toml",
    ] {
        assert!(
            walked.iter().any(|path| path == Path::new(tracked)),
            "{tracked} must be reachable without following a directory symlink: {walked:?}"
        );
    }
}

/// The snapshot must look like a git work tree to a **library-based** repository
/// reader: ai-rulez resolves rule globs against tracked files, and with no
/// `.git` falls back to a hidden-skipping walk, which reported a glob over
/// `.agents/` as `AR101` even though the file is in the snapshot (Goldziher/poly#45).
#[test]
fn the_snapshot_carries_a_git_pointer_to_the_original_repository() {
    let repo = repo();
    let cache = TempDir::new().unwrap();
    let snap = snapshot(repo.path(), cache.path(), &[PathBuf::from("pkg/a.py")], &[]);

    let pointer = std::fs::read_to_string(snap.path().join(".git")).expect("a .git pointer");
    let git_dir = git_output(repo.path(), &["rev-parse", "--absolute-git-dir"]);
    assert_eq!(pointer, format!("gitdir: {git_dir}\n"));
    assert!(
        !snap.path().join(".git").is_dir(),
        "a directory pointer would be a second repository, not the real one"
    );

    let toplevel = git_output(snap.path(), &["rev-parse", "--show-toplevel"]);
    assert_eq!(
        std::fs::canonicalize(toplevel).unwrap(),
        std::fs::canonicalize(snap.path()).unwrap(),
        "git must see the snapshot as the work tree"
    );
}

/// A hook that ran `git init` in the snapshot left a real `.git` directory;
/// the next refresh must restore the pointer rather than keep a second, empty
/// repository.
#[test]
fn a_refresh_replaces_a_snapshot_local_git_directory_with_the_pointer() {
    let repo = repo();
    let cache = TempDir::new().unwrap();
    let files = [PathBuf::from("pkg/a.py")];
    let snap = snapshot(repo.path(), cache.path(), &files, &[]);
    let pointer = std::fs::read_to_string(snap.path().join(".git")).unwrap();

    std::fs::remove_file(snap.path().join(".git")).unwrap();
    std::fs::create_dir(snap.path().join(".git")).unwrap();

    snapshot(repo.path(), cache.path(), &files, &[]);

    assert_eq!(std::fs::read_to_string(snap.path().join(".git")).unwrap(), pointer);
}

fn git_output(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git").args(args).current_dir(repo).output().expect("git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// The mirror is a hardlink, not a copy: unchanged content costs an inode
/// reference, keeps the worktree mtime, and reflects a worktree edit without a
/// refresh.
#[test]
fn mirrored_files_are_hardlinks_not_copies() {
    let repo = repo();
    let cache = TempDir::new().unwrap();
    let files = [PathBuf::from("pkg/a.py")];

    let snap = snapshot(repo.path(), cache.path(), &files, &[]);
    let mirrored = snap.path().join("other/c.py");
    let live = repo.path().join("other/c.py");

    assert!(is_hardlinked(snap.path(), repo.path(), "other/c.py"));
    assert_eq!(
        std::fs::metadata(&mirrored).unwrap().modified().unwrap(),
        std::fs::metadata(&live).unwrap().modified().unwrap(),
        "a hardlink shares the mtime it was linked with"
    );
    std::fs::write(&live, "c = 2  # written in the worktree\n").unwrap();
    assert_eq!(
        std::fs::read_to_string(&mirrored).unwrap(),
        "c = 2  # written in the worktree\n",
        "the mirror is the worktree file, not a snapshot-time copy"
    );
}

#[test]
fn a_mirror_whose_worktree_directory_is_gone_is_written_from_the_index() {
    let repo = repo();
    let cache = TempDir::new().unwrap();
    let files = [PathBuf::from("pkg/a.py")];
    // An unstaged deletion of the whole directory: the index still tracks it.
    std::fs::remove_dir_all(repo.path().join("other")).unwrap();

    let snap = snapshot(repo.path(), cache.path(), &files, &[]);

    assert!(is_written(snap.path(), "other/c.py"), "a real file, not a link");
    assert_eq!(
        std::fs::read_to_string(snap.path().join("other/c.py")).unwrap(),
        "c = 1\n",
        "the index blob, since the worktree has nothing to hardlink"
    );
    assert!(
        !repo.path().join("other").exists(),
        "the snapshot must not resurrect a worktree directory"
    );
}

#[test]
fn a_mirror_the_selection_keeps_is_not_recreated_on_refresh() {
    let repo = repo();
    let cache = TempDir::new().unwrap();
    let files = [PathBuf::from("pkg/a.py")];
    let snap = snapshot(repo.path(), cache.path(), &files, &[]);
    let created = inode(&snap.path().join("other/c.py"));
    let directory = inode(&snap.path().join("other"));

    snapshot(repo.path(), cache.path(), &files, &[]);

    assert_eq!(
        inode(&snap.path().join("other/c.py")),
        created,
        "a steady-state refresh must not churn the hardlink"
    );
    assert_eq!(
        inode(&snap.path().join("other")),
        directory,
        "nor the directory that holds it"
    );
}

#[test]
fn a_directory_that_becomes_selected_is_not_written_through_its_old_link() {
    let repo = repo();
    let cache = TempDir::new().unwrap();
    snapshot(repo.path(), cache.path(), &[PathBuf::from("pkg/a.py")], &[]);
    write(repo.path(), "other/c.py", "c = 99  # unstaged edit\n");

    let snap = snapshot(repo.path(), cache.path(), &[PathBuf::from("other/c.py")], &[]);

    assert!(is_written(snap.path(), "other/c.py"));
    assert_eq!(
        std::fs::read_to_string(snap.path().join("other/c.py")).unwrap(),
        "c = 1\n"
    );
    assert_eq!(
        std::fs::read_to_string(repo.path().join("other/c.py")).unwrap(),
        "c = 99  # unstaged edit\n",
        "the live worktree must never be written through a stale link"
    );
}

#[test]
fn sparse_snapshot_takes_content_from_the_index_not_the_worktree() {
    let repo = repo();
    let cache = TempDir::new().unwrap();
    write(repo.path(), "pkg/a.py", "a = 2\n");
    git(repo.path(), &["add", "pkg/a.py"]);
    write(repo.path(), "pkg/a.py", "a = 3  # unstaged\n");

    let snap = snapshot(repo.path(), cache.path(), &[PathBuf::from("pkg/a.py")], &[]);

    assert_eq!(
        std::fs::read_to_string(snap.path().join("pkg/a.py")).unwrap(),
        "a = 2\n"
    );
}

#[test]
fn pinned_paths_are_materialized_with_their_siblings() {
    let repo = repo();
    let cache = TempDir::new().unwrap();
    let pinned = ["scripts/check.sh".to_string()];

    let snap = snapshot(repo.path(), cache.path(), &[PathBuf::from("pkg/a.py")], &pinned);

    assert!(is_written(snap.path(), "scripts/check.sh"));
    assert!(is_written(snap.path(), "scripts/lib.sh"), "sibling of a pinned script");
    assert!(is_hardlinked(snap.path(), repo.path(), "other/c.py"));
}

#[test]
fn a_mirror_drops_a_path_the_index_no_longer_tracks() {
    let repo = repo();
    let cache = TempDir::new().unwrap();
    let files = [PathBuf::from("pkg/a.py")];
    let snap = snapshot(repo.path(), cache.path(), &files, &[]);
    assert!(is_hardlinked(snap.path(), repo.path(), "other/deep/d.py"));

    git(repo.path(), &["rm", "-q", "other/deep/d.py"]);
    snapshot(repo.path(), cache.path(), &files, &[]);

    assert!(
        !snap.path().join("other/deep/d.py").exists(),
        "a mirror must not keep serving a path the index dropped"
    );
    assert!(
        is_hardlinked(snap.path(), repo.path(), "other/c.py"),
        "and the rest of the mirror is untouched"
    );
}

#[test]
fn sparse_and_full_snapshots_do_not_share_a_directory() {
    let repo = repo();
    let cache = TempDir::new().unwrap();
    let files = [PathBuf::from("pkg/a.py")];

    let full = StagedSnapshot::create_in(cache.path(), repo.path(), &[]).unwrap();
    let sparse = StagedSnapshot::create_scoped_in(cache.path(), repo.path(), &[], sparse(&files, &[])).unwrap();

    assert_ne!(full.path(), sparse.path());
    assert!(is_written(full.path(), "other/c.py"), "the full snapshot is untouched");
}

#[test]
fn referenced_paths_extracts_path_like_words_from_command_lines() {
    let mut hook = Hook::run(
        "h",
        "sh ./scripts/check.sh --config=conf/x.toml \"lib/y.py\"; echo $HOME",
    );
    hook.args = vec!["-v".into(), "extra/z.txt".into()];

    let words = referenced_paths(&[hook]);

    for expected in ["scripts/check.sh", "conf/x.toml", "lib/y.py", "extra/z.txt"] {
        assert!(words.iter().any(|word| word == expected), "{expected} in {words:?}");
    }
    assert!(!words.iter().any(|word| word.starts_with(['-', '$'])), "{words:?}");
}

fn workspace_hook(glob: &str) -> Hook {
    let mut hook = Hook::run("ws", "true");
    hook.workspace = true;
    hook.pass_filenames = false;
    hook.files = Some(FilePattern::glob(vec![glob.to_string()]).unwrap());
    hook
}

fn request(files: &[&str]) -> HookRunRequest {
    HookRunRequest {
        root: PathBuf::from("."),
        files: files.iter().map(PathBuf::from).collect(),
        ..HookRunRequest::default()
    }
}

fn spec(hooks: Vec<Hook>) -> StageSpec {
    StageSpec {
        stage: Stage::PreCommit,
        hooks,
        ..StageSpec::default()
    }
}

#[test]
fn a_workspace_hook_with_no_matching_file_does_not_require_the_full_tree() {
    let stage = spec(vec![workspace_hook("**/*.rs")]);
    assert!(!poly_hooks::runner::requires_full_tree(&request(&["a.py"]), &stage));
    assert!(poly_hooks::runner::requires_full_tree(&request(&["a.rs"]), &stage));
}

#[test]
fn per_file_hooks_never_require_the_full_tree() {
    let mut hook = Hook::run("fmt", "true");
    hook.files = Some(FilePattern::glob(vec!["**/*.py".to_string()]).unwrap());
    assert!(!poly_hooks::runner::requires_full_tree(
        &request(&["a.py"]),
        &spec(vec![hook])
    ));
}

fn run_in_snapshot(repo: &Path, snapshot: &Path, files: &[PathBuf], hook: Hook) -> anyhow::Result<()> {
    poly_hooks::run(HookRunRequest {
        root: repo.to_path_buf(),
        work_root: Some(snapshot.to_path_buf()),
        files: files.to_vec(),
        stages: vec![spec(vec![hook])],
        ..HookRunRequest::default()
    })
    .map(|_| ())
}

fn shell_hook(command: &str) -> Hook {
    let mut hook = Hook::run("writer", command);
    hook.pass_filenames = false;
    hook.always_run = true;
    hook
}

#[test]
fn a_hook_writing_through_a_link_into_the_worktree_fails_the_run_and_names_the_directory() {
    let repo = repo();
    let cache = TempDir::new().unwrap();
    let files = [PathBuf::from("pkg/a.py")];
    let snap = snapshot(repo.path(), cache.path(), &files, &[]);

    let outcome = run_in_snapshot(
        repo.path(),
        snap.path(),
        &files,
        shell_hook("echo changed >> other/c.py"),
    );

    let error = outcome.expect_err("a write through a link must fail the run");
    let message = format!("{error:#}");
    assert!(message.contains("other"), "{message}");
    assert!(message.contains("snapshot"), "{message}");
}

#[test]
fn a_hook_creating_a_file_in_a_mirrored_directory_only_touches_the_snapshot() {
    let repo = repo();
    let cache = TempDir::new().unwrap();
    let files = [PathBuf::from("pkg/a.py")];
    let snap = snapshot(repo.path(), cache.path(), &files, &[]);

    // A mirrored directory is a real directory inside the snapshot, so a new
    // file lands there — never in the author's worktree.
    run_in_snapshot(repo.path(), snap.path(), &files, shell_hook("echo x > other/new.txt")).unwrap();

    assert!(snap.path().join("other/new.txt").is_file());
    assert!(
        !repo.path().join("other/new.txt").exists(),
        "an untracked write must not reach the worktree"
    );
}

#[test]
fn a_hook_that_writes_only_inside_the_snapshot_passes() {
    let repo = repo();
    let cache = TempDir::new().unwrap();
    let files = [PathBuf::from("pkg/a.py")];
    let snap = snapshot(repo.path(), cache.path(), &files, &[]);

    run_in_snapshot(repo.path(), snap.path(), &files, shell_hook("echo changed >> pkg/a.py")).unwrap();

    assert_eq!(
        std::fs::read_to_string(snap.path().join("pkg/a.py")).unwrap(),
        "a = 1\nchanged\n",
        "the hook wrote into the snapshot's own copy, which is not a link to the worktree"
    );
}

#[test]
fn a_ruff_config_extending_a_file_outside_the_directory_chain_gets_it_from_the_index() {
    let repo = repo();
    let root = repo.path();
    write(root, "pkg/ruff.toml", "extend = \"../shared/base.toml\"\n");
    write(
        root,
        "shared/base.toml",
        "extend = \"../more/other.toml\"\nline-length = 100\n",
    );
    write(root, "more/other.toml", "indent-width = 2\n");
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "configs"]);
    write(root, "shared/base.toml", "line-length = 7  # unstaged\n");
    let cache = TempDir::new().unwrap();

    let snap = snapshot(root, cache.path(), &[PathBuf::from("pkg/a.py")], &[]);

    assert!(
        is_written(snap.path(), "shared/base.toml"),
        "the extended file is materialized"
    );
    assert_eq!(
        std::fs::read_to_string(snap.path().join("shared/base.toml")).unwrap(),
        "extend = \"../more/other.toml\"\nline-length = 100\n",
        "from the index, not the unstaged edit"
    );
    assert!(is_written(snap.path(), "more/other.toml"), "so is the file it extends");
    assert!(
        is_hardlinked(snap.path(), root, "other/c.py"),
        "unrelated directories stay mirrored, not written from the index"
    );
}

#[test]
fn a_pyproject_ruff_extend_is_followed_and_an_escaping_one_is_ignored() {
    let repo = repo();
    let root = repo.path();
    write(
        root,
        "pkg/pyproject.toml",
        "[tool.ruff]\nextend = \"../shared/base.toml\"\n",
    );
    write(root, "shared/base.toml", "line-length = 100\n");
    write(root, "pkg/sub/ruff.toml", "extend = \"../../../outside.toml\"\n");
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "configs"]);
    let cache = TempDir::new().unwrap();

    let snap = snapshot(root, cache.path(), &[PathBuf::from("pkg/sub/b.py")], &[]);

    assert!(is_written(snap.path(), "shared/base.toml"));
}

fn index_flags(repo: &Path) -> String {
    let output = Command::new("git")
        .args(["ls-files", "-v", "--stage"])
        .current_dir(repo)
        .output()
        .expect("git ls-files");
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap()
}

/// A cone sparse checkout of `included/`, with `omitted/` tracked but absent
/// from the worktree, and `included/a.txt` staged with a later unstaged edit.
fn cone_sparse_repo() -> TempDir {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "t@example.com"]);
    git(root, &["config", "user.name", "T"]);
    git(root, &["config", "commit.gpgsign", "false"]);
    write(
        root,
        "proof.sh",
        "grep -qx kept omitted/b.txt && grep -qx staged included/a.txt\n",
    );
    write(root, "included/a.txt", "base\n");
    write(root, "omitted/b.txt", "kept\n");
    write(root, "omitted/deep/c.txt", "deep\n");
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "init"]);
    git(root, &["sparse-checkout", "init", "--cone"]);
    git(root, &["sparse-checkout", "set", "included"]);
    write(root, "included/a.txt", "staged\n");
    git(root, &["add", "included/a.txt"]);
    write(root, "included/a.txt", "unstaged\n");
    assert!(!root.join("omitted").exists(), "the fixture omits a directory");
    dir
}

#[test]
fn a_cone_sparse_checkouts_omitted_files_are_written_from_the_index() {
    let repo = cone_sparse_repo();
    let root = repo.path();
    let cache = TempDir::new().unwrap();
    let flags = index_flags(root);
    assert!(
        flags.contains("S 100644 ") && flags.contains("\tomitted/b.txt"),
        "{flags}"
    );

    let snap = snapshot(root, cache.path(), &[PathBuf::from("included/a.txt")], &[]);

    for (path, content) in [
        ("omitted/b.txt", "kept\n"),
        ("omitted/deep/c.txt", "deep\n"),
        ("included/a.txt", "staged\n"),
    ] {
        assert!(is_written(snap.path(), path), "{path} should be materialized");
        assert_eq!(
            std::fs::read_to_string(snap.path().join(path)).unwrap(),
            content,
            "{path}"
        );
    }
    assert!(
        !root.join("omitted").exists(),
        "the live sparse checkout is not expanded"
    );
    assert_eq!(index_flags(root), flags, "skip-worktree bits and OIDs are preserved");
    assert_eq!(
        std::fs::read_to_string(root.join("included/a.txt")).unwrap(),
        "unstaged\n"
    );
}

#[test]
fn a_hook_in_a_sparse_snapshot_reads_an_omitted_tracked_file() {
    let repo = cone_sparse_repo();
    let root = repo.path();
    let cache = TempDir::new().unwrap();
    let files = [PathBuf::from("included/a.txt")];
    // The command names only the script, so nothing pins `omitted/`.
    let hook = shell_hook("sh proof.sh");
    let pinned = referenced_paths(std::slice::from_ref(&hook));
    let snap = snapshot(root, cache.path(), &files, &pinned);

    let outcome = poly_hooks::run(HookRunRequest {
        root: root.to_path_buf(),
        work_root: Some(snap.path().to_path_buf()),
        files: files.to_vec(),
        stages: vec![spec(vec![hook])],
        ..HookRunRequest::default()
    })
    .unwrap();

    assert!(outcome.success(), "the hook sees the omitted file and the staged bytes");
    assert!(
        !root.join("omitted").exists(),
        "the live sparse checkout is not expanded"
    );
}

#[test]
fn a_skip_worktree_file_is_read_from_the_index_not_through_a_link() {
    // `git update-index --skip-worktree` on a full checkout leaves the worktree
    // copy in place, and git stops comparing it with the index, so it may hold
    // anything. A link to its directory would expose those bytes.
    let repo = repo();
    let root = repo.path();
    git(root, &["update-index", "--skip-worktree", "other/c.py"]);
    write(root, "other/c.py", "c = 99  # ignored by git\n");
    let cache = TempDir::new().unwrap();

    let snap = snapshot(root, cache.path(), &[PathBuf::from("pkg/a.py")], &[]);

    assert!(is_written(snap.path(), "other/c.py"));
    assert_eq!(
        std::fs::read_to_string(snap.path().join("other/c.py")).unwrap(),
        "c = 1\n"
    );
    assert!(
        snap.path().join("other/deep/d.py").is_file(),
        "a subdirectory of a now-written directory still resolves"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("other/c.py")).unwrap(),
        "c = 99  # ignored by git\n"
    );
}

#[test]
fn a_snapshot_include_inside_a_mirrored_directory_leaves_the_worktree_file_alone() {
    // `other/` is mirrored, so `<snapshot>/other/` is a real directory of
    // hardlinks; the untracked entry is linked into it on its own. Replacing it
    // with a link must never delete the author's untracked file.
    let repo = repo();
    let root = repo.path();
    write(root, "other/gen.txt", "generated\n");
    let cache = TempDir::new().unwrap();
    let files = [PathBuf::from("pkg/a.py")];
    let includes = ["other/gen.txt".to_string()];

    let snap = StagedSnapshot::create_scoped_in(cache.path(), root, &includes, sparse(&files, &[])).unwrap();

    let live = std::fs::symlink_metadata(root.join("other/gen.txt")).unwrap();
    assert!(live.file_type().is_file(), "the worktree file is still a file");
    assert_eq!(
        std::fs::read_to_string(root.join("other/gen.txt")).unwrap(),
        "generated\n"
    );
    assert_eq!(
        std::fs::read_to_string(snap.path().join("other/gen.txt")).unwrap(),
        "generated\n",
        "the entry is reachable through the directory link"
    );

    StagedSnapshot::create_scoped_in(cache.path(), root, &[], sparse(&files, &[])).unwrap();

    assert!(
        root.join("other/gen.txt").is_file(),
        "withdrawing the entry does not touch the worktree either"
    );
}

#[test]
fn withdrawing_an_include_never_unlinks_a_worktree_symlink_inside_a_mirrored_directory() {
    let repo = repo();
    let root = repo.path();
    std::os::unix::fs::symlink("c.py", root.join("other/alias.py")).unwrap();
    let cache = TempDir::new().unwrap();
    let files = [PathBuf::from("pkg/a.py")];
    // Recorded while `other/` was written (a file in it was staged)...
    let includes = ["other/alias.py".to_string()];
    StagedSnapshot::create_scoped_in(
        cache.path(),
        root,
        &includes,
        sparse(&[PathBuf::from("other/c.py")], &[]),
    )
    .unwrap();

    // ...then withdrawn while `other/` is linked.
    StagedSnapshot::create_scoped_in(cache.path(), root, &[], sparse(&files, &[])).unwrap();

    assert!(
        std::fs::symlink_metadata(root.join("other/alias.py"))
            .unwrap()
            .file_type()
            .is_symlink(),
        "the author's own symlink survives"
    );
}
