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

fn sparse<'a>(files: &'a [PathBuf], pinned: &'a [String]) -> SnapshotScope<'a> {
    SnapshotScope::Sparse { files, pinned }
}

fn snapshot(repo: &Path, cache: &Path, files: &[PathBuf], pinned: &[String]) -> StagedSnapshot {
    StagedSnapshot::create_scoped_in(cache, repo, &[], sparse(files, pinned)).unwrap()
}

#[test]
fn sparse_snapshot_holds_staged_files_and_directory_context_only() {
    let repo = repo();
    let cache = TempDir::new().unwrap();
    let files = [PathBuf::from("pkg/sub/b.py")];

    let snap = snapshot(repo.path(), cache.path(), &files, &[]);

    for present in ["pkg/sub/b.py", "pkg/__init__.py", "pkg/a.py", "pyproject.toml"] {
        assert!(is_written(snap.path(), present), "{present} should be materialized");
    }
    for absent in ["other/c.py", "other/deep/d.py", "scripts/check.sh"] {
        assert!(!is_written(snap.path(), absent), "{absent} should not be written");
    }
}

#[test]
fn unselected_directories_are_linked_so_existence_probes_see_the_tree() {
    let repo = repo();
    let cache = TempDir::new().unwrap();
    let files = [PathBuf::from("pkg/sub/b.py")];

    let snap = snapshot(repo.path(), cache.path(), &files, &[]);

    let other = snap.path().join("other");
    assert!(std::fs::symlink_metadata(&other).unwrap().file_type().is_symlink());
    assert!(
        snap.path().join("other/deep").is_dir(),
        "a nested directory resolves through the link"
    );
    assert!(snap.path().join("other/c.py").is_file(), "so does a module file");
}

#[test]
fn a_link_the_selection_keeps_survives_a_refresh() {
    let repo = repo();
    let cache = TempDir::new().unwrap();
    let files = [PathBuf::from("pkg/a.py")];
    let snap = snapshot(repo.path(), cache.path(), &files, &[]);
    let before = std::fs::read_link(snap.path().join("other")).unwrap();
    let inode = |path: &Path| std::os::unix::fs::MetadataExt::ino(&std::fs::symlink_metadata(path).unwrap());
    let created = inode(&snap.path().join("other"));

    snapshot(repo.path(), cache.path(), &files, &[]);

    assert_eq!(std::fs::read_link(snap.path().join("other")).unwrap(), before);
    assert_eq!(inode(&snap.path().join("other")), created, "the link was not recreated");
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
    assert!(!is_written(snap.path(), "other/c.py"));
}

#[test]
fn files_that_leave_the_selection_are_pruned_on_refresh() {
    let repo = repo();
    let cache = TempDir::new().unwrap();
    let snap = snapshot(repo.path(), cache.path(), &[PathBuf::from("other/deep/d.py")], &[]);
    assert!(is_written(snap.path(), "other/deep/d.py"));

    snapshot(repo.path(), cache.path(), &[PathBuf::from("pkg/a.py")], &[]);

    assert!(!is_written(snap.path(), "other/deep/d.py"));
    assert!(is_written(snap.path(), "pkg/a.py"));
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
