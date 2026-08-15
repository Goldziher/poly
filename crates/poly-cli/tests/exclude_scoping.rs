//! `[discovery] exclude` must hold whatever path the run is *pointed at*.
//!
//! A whole-repository walk (`poly fmt --fix .`) prunes excluded paths at the walk
//! boundary, so the exclude is obviously in force. Naming a path below the
//! excluded tree takes a different route: the run root's excludes are re-anchored
//! from the config's directory to the *walk root* before `--force-exclude` matches
//! the named path against them. Two shapes fell through that re-anchoring and the
//! excluded file was rewritten in place:
//!
//! - a glob pruning an **ancestor** of the named path (`packages/dart/**` while
//!   linting `packages/dart/lib/`) was classified as a sibling subtree and dropped;
//! - a named **file** was anchored as though the file itself were the walk root's
//!   directory, so every glob written against its parent directory missed.
//!
//! Both are reachable from the pre-commit hook path, which never names a directory
//! — it is always handed explicit staged file paths — so an exclude that a repo
//! could see working under `poly fmt --check .` was inert on the files that
//! actually got committed. These tests assert the bytes on disk, not the report:
//! the defect *is* the write.
#![cfg(unix)]

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

const POLY: &str = env!("CARGO_BIN_EXE_poly");

/// Unformatted on purpose: any engine that sees this file rewrites it, so an
/// unchanged hash is proof no engine was offered it.
const MESSY: &str = "def  f( x ):\n      return   x+1\n";

/// A repo whose only config is a root `poly.toml` carrying `exclude`, with one
/// messy file nested two directories deep.
fn repo(exclude: &str) -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::create_dir_all(root.join("packages/dart/lib")).expect("mkdir");
    std::fs::write(
        root.join("poly.toml"),
        format!("[workspace]\nroot = true\n\n[discovery]\nexclude = [\"{exclude}\"]\n"),
    )
    .expect("write poly.toml");
    std::fs::write(root.join("packages/dart/lib/two.py"), MESSY).expect("write two.py");
    dir
}

/// Run `poly fmt --fix` against `arg` from `root` and return the target's bytes.
fn fmt_fix(root: &Path, arg: &str) -> String {
    let status = Command::new(POLY)
        .args(["fmt", "--fix", "--no-cache", arg])
        .current_dir(root)
        .output()
        .expect("run poly");
    // `fmt --fix` exits non-zero when it rewrote something; either way the bytes
    // on disk are the assertion, so the status is only reported on panic.
    let _ = status;
    std::fs::read_to_string(root.join("packages/dart/lib/two.py")).expect("read two.py")
}

/// `packages/dart/**` prunes the whole package. Naming a directory *inside* it
/// must not smuggle the excluded file back into the run.
#[test]
fn ancestor_anchored_exclude_holds_for_a_named_directory() {
    let dir = repo("packages/dart/**");
    assert_eq!(
        fmt_fix(dir.path(), "packages/dart/lib"),
        MESSY,
        "a directory inside an excluded tree must not be formatted"
    );
}

/// The same glob, with the excluded file itself named — the pre-commit hook shape.
#[test]
fn ancestor_anchored_exclude_holds_for_a_named_file() {
    let dir = repo("packages/dart/**");
    assert_eq!(
        fmt_fix(dir.path(), "packages/dart/lib/two.py"),
        MESSY,
        "a file inside an excluded tree must not be formatted"
    );
}

/// A glob anchored at the file's own parent directory. This one works when the
/// directory is named and used to fail when the file was, which is precisely the
/// difference between a developer's `poly fmt --check .` and their commit hook.
#[test]
fn parent_anchored_exclude_holds_for_a_named_file() {
    let dir = repo("packages/dart/lib/*.py");
    assert_eq!(
        fmt_fix(dir.path(), "packages/dart/lib/two.py"),
        MESSY,
        "a file matched by a parent-anchored exclude must not be formatted"
    );
}

/// The counterweight: a named file the exclude set does *not* match is still
/// formatted, so none of the above can be satisfied by excluding everything.
#[test]
fn a_file_outside_the_exclude_is_still_formatted() {
    let dir = repo("packages/other/**");
    let formatted = fmt_fix(dir.path(), "packages/dart/lib/two.py");
    assert_ne!(
        formatted, MESSY,
        "a file no exclude matches must still be formatted when named"
    );
}
