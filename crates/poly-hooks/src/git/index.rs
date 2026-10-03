//! Index (staging-area) queries and materialization, plus the path-safety guard
//! that every one of them depends on.
//!
//! Split from the parent module because these helpers share one input — the git
//! **index** — and one hazard: every path they return is joined onto a
//! materialization root (the staged snapshot, the worktree) by their callers, so
//! a handcrafted index entry is an arbitrary-file-write vector. The guard
//! ([`is_safe_relative_path`] / [`reject_unsafe_path`]) therefore lives here,
//! next to the parsers and readers that must apply it, rather than in a separate
//! "validation" module where the two could drift apart.

use std::path::{Path, PathBuf};

use tracing::instrument;

use super::{Error, git_cmd, path_from_git_bytes, zsplit};

/// Whether `path` is a plain relative path that is safe to join onto a
/// materialization root (the staged snapshot, the worktree).
///
/// Only `Normal` components are accepted: an absolute path, a Windows drive
/// prefix, `.` or `..` would all let `root.join(path)` address a file outside
/// `root`. Git's own `verify_path` already refuses to record such an entry, so
/// a real repository never trips this — but the check must not be delegated to
/// the installed git binary, because a handcrafted index is exactly the input
/// an attacker controls, and every path we join comes from that index.
#[must_use]
pub fn is_safe_relative_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}

/// [`is_safe_relative_path`] as a guard, naming the path it rejected.
fn reject_unsafe_path(path: &Path) -> Result<(), Error> {
    if is_safe_relative_path(path) {
        return Ok(());
    }
    Err(Error::UnsafePath {
        path: path.display().to_string(),
    })
}

/// List files that are staged in the index (excluding deleted files).
#[instrument(level = "trace")]
pub fn get_staged_files(root: &Path) -> Result<Vec<PathBuf>, Error> {
    let output = git_cmd("get staged files")?
        .current_dir(root)
        .arg("diff")
        .arg("--cached")
        .arg("--name-only")
        .arg("--diff-filter=ACMRTUXB")
        .arg("--no-ext-diff")
        .arg("-z")
        .check(true)
        .output()?;
    Ok(zsplit(&output.stdout)?)
}

/// List every file tracked in the index (`git ls-files`).
///
/// Used by `poly hooks run --all-files` and by `pre-push` over a root-commit
/// push, where the whole tracked tree is checked rather than a diff range.
#[instrument(level = "trace")]
pub fn list_files(root: &Path) -> Result<Vec<PathBuf>, Error> {
    let output = git_cmd("list tracked files")?
        .current_dir(root)
        .arg("ls-files")
        .arg("-z")
        .check(true)
        .output()?;
    Ok(zsplit(&output.stdout)?)
}

/// Largest number of path arguments to pass to a single `git checkout-index`
/// invocation. Batching keeps the argument vector well under the OS `ARG_MAX`
/// limit on repositories with tens of thousands of files.
const CHECKOUT_BATCH: usize = 1000;

/// Materialize the staged (index) content of specific `paths` into `dest`.
///
/// Runs `git checkout-index -f --ignore-skip-worktree-bits --prefix=<dest>/ -- <paths>`, which writes each
/// listed entry's **index blob** — i.e. exactly the staged content — beneath
/// `dest`, recreating the repo-relative directory tree (leading directories are
/// created, exec bits and symlinks are reproduced faithfully). Untracked files
/// and unstaged worktree edits are never written, so the result is a
/// byte-faithful, non-destructive copy of what a commit would capture; `dest`
/// must already exist. Omitted sparse entries materialize only under `dest`,
/// without expanding the live checkout. This is how whole-workspace hooks (`cargo clippy`, type
/// checkers, …) are isolated to staged content without touching the live
/// worktree. A no-op for an empty `paths`; large lists are batched to stay under
/// `ARG_MAX`.
///
/// `--ignore-skip-worktree-bits` requires git ≥ 2.6 (released 2015); it is the
/// only way to materialize a `skip-worktree` entry without expanding the live
/// checkout, so a sparse-checkout worktree depends on it.
#[instrument(level = "trace", skip(paths))]
pub fn checkout_index_paths(root: &Path, dest: &Path, paths: &[PathBuf]) -> Result<(), Error> {
    for batch in paths.chunks(CHECKOUT_BATCH) {
        let mut cmd = git_cmd("checkout staged paths")?;
        cmd.current_dir(root)
            .arg("-c")
            .arg("core.autocrlf=false")
            .arg("checkout-index")
            .arg("-f")
            .arg("--ignore-skip-worktree-bits")
            .arg(prefix_arg(dest))
            .arg("--");
        for path in batch {
            cmd.arg(path);
        }
        // `.output()` rather than `.status()` so a failure carries git's stderr
        // (missing blob, unreachable partial-clone remote, unsupported git) into
        // the error the user sees, instead of an empty captured-output block.
        cmd.check(true).output()?;
    }
    Ok(())
}

/// A single index entry: its blob OID, repo-relative path, and whether it is a
/// symlink — read straight from the index by [`list_staged_entries`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedEntry {
    /// The staged blob's object id (the content a commit would capture).
    pub oid: String,
    /// Repo-relative path of the entry.
    pub path: PathBuf,
    /// `true` when the entry is a symlink (index mode `120000`).
    pub is_symlink: bool,
}

/// List every entry staged in the index with its blob OID (`git ls-files -s`).
///
/// The OID is read directly from the index, so it is the authoritative record of
/// "what a commit would capture" and is **independent of the worktree stat
/// cache** — unlike `git diff-files`, which is stat-based and can under-report a
/// genuinely-modified file as clean when the index stat cache is stale or
/// inconsistent. The staged snapshot relies on this so that an unstaged worktree
/// edit can never leak into the materialized content. Submodule gitlinks (mode
/// `160000`) have no blob to materialize and are skipped.
#[instrument(level = "trace")]
pub fn list_staged_entries(root: &Path) -> Result<Vec<StagedEntry>, Error> {
    let output = git_cmd("list staged entries")?
        .current_dir(root)
        .arg("ls-files")
        .arg("-s")
        .arg("-z")
        .check(true)
        .output()?;
    parse_ls_files_stage(&output.stdout)
}

/// Parse `git ls-files -s -z` output: NUL-separated `<mode> <oid> <stage>\t<path>`
/// records. Robust to 40- or 64-hex OIDs and to paths containing spaces (the
/// path is taken verbatim after the tab).
///
/// A path that would escape the tree it is joined onto is a hard error, not a
/// skip: every consumer joins these paths onto a directory, and an index that
/// contains one is hostile rather than merely unusual (see
/// [`is_safe_relative_path`]).
fn parse_ls_files_stage(bytes: &[u8]) -> Result<Vec<StagedEntry>, Error> {
    let mut entries = Vec::new();
    for record in bytes.split(|&byte| byte == 0).filter(|slice| !slice.is_empty()) {
        let Some(tab) = record.iter().position(|&byte| byte == b'\t') else {
            continue;
        };
        let (meta, tab_and_path) = record.split_at(tab);
        let path = path_from_git_bytes(&tab_and_path[1..])?;
        reject_unsafe_path(&path)?;
        let meta = std::str::from_utf8(meta)?;
        let mut fields = meta.split_whitespace();
        let mode = fields.next().unwrap_or_default();
        let oid = fields.next().unwrap_or_default();
        // Only regular files (`100644`), executables (`100755`) and symlinks
        // (`120000`) have a blob to materialize. A submodule gitlink (`160000`)
        // is handled separately, and a sparse-index directory entry (`040000`)
        // is a tree rather than a blob — `list_staged_entries` does not pass
        // `--sparse`, so git expands those away, but treating one as a file
        // would hand `checkout-index` a tree OID. Skip anything non-blob rather
        // than trust the current invocation to keep them out.
        if !matches!(mode, "100644" | "100755" | "120000") {
            continue;
        }
        entries.push(StagedEntry {
            oid: oid.to_string(),
            path,
            is_symlink: mode == "120000",
        });
    }
    Ok(entries)
}

/// List every submodule gitlink path recorded in the index (`git ls-files -s`
/// entries with mode `160000`).
///
/// Submodules have no blob to materialize, so [`list_staged_entries`] skips them
/// — but whole-workspace compile hooks still need the submodule's checked-out
/// files present (e.g. a test that `include_bytes!`es a fixture from a
/// submodule), or the isolated sandbox fails to compile even though the real tree
/// does. The staged snapshot materializes these paths separately.
#[instrument(level = "trace")]
pub fn list_submodule_gitlinks(root: &Path) -> Result<Vec<PathBuf>, Error> {
    let output = git_cmd("list submodule gitlinks")?
        .current_dir(root)
        .arg("ls-files")
        .arg("-s")
        .arg("-z")
        .check(true)
        .output()?;
    parse_ls_files_gitlinks(&output.stdout)
}

/// Parse `git ls-files -s -z` output, returning only the submodule gitlink paths
/// (mode `160000`) — the complement of [`parse_ls_files_stage`], which skips them.
fn parse_ls_files_gitlinks(bytes: &[u8]) -> Result<Vec<PathBuf>, Error> {
    let mut paths = Vec::new();
    for record in bytes.split(|&byte| byte == 0).filter(|slice| !slice.is_empty()) {
        let Some(tab) = record.iter().position(|&byte| byte == b'\t') else {
            continue;
        };
        let (meta, tab_and_path) = record.split_at(tab);
        let mode = std::str::from_utf8(meta)?.split_whitespace().next().unwrap_or_default();
        if mode == "160000" {
            let path = path_from_git_bytes(&tab_and_path[1..])?;
            reject_unsafe_path(&path)?;
            paths.push(path);
        }
    }
    Ok(paths)
}

/// Build the `--prefix=<dest>/` argument for `git checkout-index`.
///
/// The prefix is prepended verbatim to each index path, so it must carry a
/// trailing separator or the first path component would be glued onto `dest`.
fn prefix_arg(dest: &Path) -> std::ffi::OsString {
    let mut prefix = dest.as_os_str().to_os_string();
    prefix.push(std::path::MAIN_SEPARATOR_STR);
    let mut arg = std::ffi::OsString::from("--prefix=");
    arg.push(&prefix);
    arg
}

/// Stage `paths` into the index (`git add -- <paths>`).
///
/// Used by `stage_fixed` to re-stage files a hook rewrote. A no-op when
/// `paths` is empty.
#[instrument(level = "trace", skip(paths))]
pub fn add(root: &Path, paths: &[PathBuf]) -> Result<(), Error> {
    if paths.is_empty() {
        return Ok(());
    }
    let mut cmd = git_cmd("git add")?;
    cmd.current_dir(root).arg("add").arg("--");
    for path in paths {
        cmd.arg(path);
    }
    cmd.check(true).status()?;
    Ok(())
}

/// Read the staged (index) bytes of `path` — the content a commit would capture
/// — or `None` when the path has no stage-0 entry.
///
/// This is the **content** answer to "does the worktree still match the index?",
/// and it deliberately replaces the `git diff-files` probes this module used to
/// expose. `diff-files` is stat-based: it can report a genuinely-modified file
/// as clean whenever the index stat cache is stale or has been suppressed
/// (`--assume-unchanged`, `--skip-worktree`, a sparse checkout, coarse mtime
/// granularity, a network filesystem, or any tool that rewrote the index
/// outside a normal `git add`). [`list_staged_entries`] already stopped trusting
/// it for the same reason; nothing that can lose a user's work may depend on it.
///
/// For a symlink entry the "blob" is the link target text, so a caller that must
/// not follow links has to check the worktree entry type separately.
#[instrument(level = "trace")]
pub fn staged_blob(root: &Path, path: &Path) -> Result<Option<Vec<u8>>, Error> {
    reject_unsafe_path(path)?;
    // `:0:<path>` names stage 0 of `path` in the index. The `:0:` prefix also
    // makes the argument un-option-like whatever the path begins with.
    let mut revision = std::ffi::OsString::from(":0:");
    revision.push(path.as_os_str());
    let output = git_cmd("read staged blob")?
        .current_dir(root)
        .arg("cat-file")
        .arg("blob")
        .arg(revision)
        .check(false)
        .output()?;

    if !output.status.success() {
        return Ok(None);
    }
    Ok(Some(output.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    use crate::git::testutil::{git_run, init_temp_repo};

    #[test]
    fn parse_ls_files_stage_reads_oid_path_and_symlink() {
        let input = b"100644 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 0\tsrc/a b.rs\0\
120000 bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb 0\tlink\0\
160000 cccccccccccccccccccccccccccccccccccccccc 0\tvendored\0";
        let entries = parse_ls_files_stage(input).expect("parse");
        assert_eq!(entries.len(), 2, "submodule gitlink must be skipped");
        assert_eq!(entries[0].oid, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        assert_eq!(entries[0].path, PathBuf::from("src/a b.rs"));
        assert!(!entries[0].is_symlink);
        assert_eq!(entries[1].path, PathBuf::from("link"));
        assert!(entries[1].is_symlink, "mode 120000 is a symlink");
    }

    #[test]
    fn parse_ls_files_stage_skips_directory_entries() {
        // A sparse index (`index.sparse=true`) answers `ls-files -s --sparse`
        // with collapsed `040000 <tree-oid>` directory records. poly does not
        // pass `--sparse`, so git expands them away — but treating one as a file
        // would hand `checkout-index` a tree OID, so the parser must skip it.
        let input = b"100644 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 0\tsrc/a.rs\0\
040000 cccccccccccccccccccccccccccccccccccccccc 0\tomitted\0";
        let entries = parse_ls_files_stage(input).expect("parse");
        assert_eq!(entries.len(), 1, "a 040000 directory entry must be skipped");
        assert_eq!(entries[0].path, PathBuf::from("src/a.rs"));
    }

    #[test]
    fn checkout_failure_carries_gits_stderr() {
        // A missing blob (a partial clone with no reachable remote) is the
        // failure a user is most likely to hit, and the actionable text —
        // which path, which remedy — is git's, not poly's. The error must carry
        // it rather than an empty captured-output block.
        let repo = init_temp_repo();
        let root = repo.path();
        std::fs::write(root.join("a.rs"), "fn main() {}\n").expect("write");
        git_run(root, &["add", "a.rs"]);
        // Point the index at an OID no object store has, so the checkout must
        // fail reading the blob. `update-index` does not verify object
        // existence, so this is a faithful stand-in for a pruned/missing blob.
        let missing = Command::new("git")
            .args([
                "update-index",
                "--cacheinfo",
                "100644",
                "1111111111111111111111111111111111111111",
                "a.rs",
            ])
            .current_dir(root)
            .output()
            .expect("git update-index");
        assert!(missing.status.success(), "fixture setup failed");

        let dest = tempfile::TempDir::new().expect("dest");
        let error =
            checkout_index_paths(root, dest.path(), &[PathBuf::from("a.rs")]).expect_err("a missing blob must fail");
        let rendered = error.to_string();
        assert!(
            rendered.contains("unable to read") || rendered.contains("a.rs"),
            "checkout error must surface git's own message, got: {rendered}"
        );
    }

    #[test]
    fn parse_ls_files_gitlinks_returns_only_submodules() {
        let input = b"100644 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 0\tsrc/a.rs\0\
160000 cccccccccccccccccccccccccccccccccccccccc 0\tvendor/sub\0\
160000 dddddddddddddddddddddddddddddddddddddddd 0\ttest documents\0";
        let paths = parse_ls_files_gitlinks(input).expect("parse");
        assert_eq!(
            paths,
            vec![PathBuf::from("vendor/sub"), PathBuf::from("test documents")],
            "only submodule gitlinks, paths verbatim (incl. spaces)"
        );
    }

    #[test]
    fn index_paths_that_escape_their_tree_are_rejected() {
        assert!(is_safe_relative_path(Path::new("src/a.rs")));
        assert!(is_safe_relative_path(Path::new("dir/a file.rs")));
        assert!(!is_safe_relative_path(Path::new("")));
        assert!(!is_safe_relative_path(Path::new("../outside.rs")));
        assert!(!is_safe_relative_path(Path::new("src/../../outside.rs")));
        assert!(!is_safe_relative_path(Path::new("./a.rs")));
        assert!(!is_safe_relative_path(Path::new("/etc/passwd")));

        // A handcrafted index is the attacker-controlled input here, so the
        // parsers must reject rather than join the path onto a root.
        let escaping = b"100644 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 0\t../../.bashrc\0";
        assert!(matches!(parse_ls_files_stage(escaping), Err(Error::UnsafePath { .. })));
        let absolute = b"100644 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 0\t/etc/passwd\0";
        assert!(matches!(parse_ls_files_stage(absolute), Err(Error::UnsafePath { .. })));
        let gitlink = b"160000 cccccccccccccccccccccccccccccccccccccccc 0\t../evil\0";
        assert!(matches!(
            parse_ls_files_gitlinks(gitlink),
            Err(Error::UnsafePath { .. })
        ));
        assert!(matches!(
            staged_blob(Path::new("."), Path::new("../escape")),
            Err(Error::UnsafePath { .. })
        ));
    }

    #[test]
    fn staged_blob_reads_index_content_not_the_worktree() {
        let repo = init_temp_repo();
        let root = repo.path();
        std::fs::write(root.join("a.txt"), "staged\n").expect("write");
        git_run(root, &["add", "a.txt"]);
        std::fs::write(root.join("a.txt"), "worktree edit\n").expect("write");

        let blob = staged_blob(root, Path::new("a.txt")).expect("staged blob");
        assert_eq!(blob.as_deref(), Some(&b"staged\n"[..]));
        assert_eq!(
            staged_blob(root, Path::new("absent.txt")).expect("absent path"),
            None,
            "a path with no stage-0 entry has no staged bytes"
        );
    }

    /// The whole reason [`staged_blob`] exists: `git diff-files` under
    /// `--assume-unchanged` calls a genuinely-modified file clean, while reading
    /// the index content still sees the difference.
    #[test]
    fn staged_blob_sees_a_difference_the_stat_cache_hides() {
        let repo = init_temp_repo();
        let root = repo.path();
        std::fs::write(root.join("a.txt"), "staged\n").expect("write");
        git_run(root, &["add", "a.txt"]);
        std::fs::write(root.join("a.txt"), "staged\nunstaged work\n").expect("write");
        git_run(root, &["update-index", "--assume-unchanged", "a.txt"]);

        let diff_files_clean = Command::new("git")
            .args(["diff-files", "--quiet", "--", "a.txt"])
            .current_dir(root)
            .status()
            .expect("git diff-files")
            .success();
        assert!(diff_files_clean, "precondition: the stat cache reports this file clean");

        let blob = staged_blob(root, Path::new("a.txt")).expect("staged blob");
        assert_eq!(
            blob.as_deref(),
            Some(&b"staged\n"[..]),
            "the index content must be visible even when the stat cache lies"
        );
    }
}
