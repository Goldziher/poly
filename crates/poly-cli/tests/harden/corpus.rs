//! Resolving what to measure, and keeping a source tree read-only while doing it.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::record::Corpus;

/// One root the harness will measure.
pub struct Root {
    pub name: String,
    pub path: PathBuf,
    pub corpus: Corpus,
    pub commit: Option<String>,
    pub dirty: bool,
    /// See [`tree_state`]; taken before the measurement starts.
    pub tree_state: Option<String>,
}

/// Read the root to measure out of the environment, the way `scripts/harden.sh`
/// hands it over.
///
/// Returns `None` when the variables are absent, which is how the ignored test
/// stays runnable (and compilable) under a plain `cargo test`.
pub fn from_env() -> Option<Root> {
    let path = PathBuf::from(std::env::var("POLY_HARDEN_ROOT_PATH").ok()?);
    let name = std::env::var("POLY_HARDEN_ROOT_NAME").unwrap_or_else(|_| {
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unnamed".to_string())
    });
    let corpus = match std::env::var("POLY_HARDEN_CORPUS").as_deref() {
        Ok("a") => Corpus::A,
        Ok("c") => Corpus::C,
        _ => Corpus::B,
    };
    let commit = git(&path, &["rev-parse", "HEAD"]);
    let dirty = is_dirty(&path);
    let tree_state = tree_state(&path);
    Some(Root {
        name,
        path,
        corpus,
        commit,
        dirty,
        tree_state,
    })
}

/// Whether the tree has uncommitted changes *right now*. Reported so a reader
/// knows the root's counts are not comparable with anything.
pub fn is_dirty(root: &Path) -> bool {
    git(root, &["status", "--porcelain"]).is_some_and(|out| !out.trim().is_empty())
}

/// A cheap fingerprint of the tree's uncommitted state.
///
/// Taken before and after the measurement, because a live working tree can
/// change while the harness reads it, and a cache comparison that spans that
/// change reports a disagreement that is a fact about the tree rather than about
/// poly. Without this the two are indistinguishable and the harness would invent
/// a cache defect out of somebody saving a file.
///
/// A boolean "is it dirty" is not enough: a tree that was dirty before and after
/// but changed in between reads as unmoved, which is the likeliest case of all —
/// somebody is working in it. `--numstat` is what catches that, since editing a
/// tracked file moves its line counts while its porcelain status line stays
/// `` M ``.
///
/// `None` on a root that is not a git checkout. Movement is undetectable there,
/// which is a limit of the corpus and not something to paper over: the two
/// `None`s compare equal, so such a root is treated as unmoved and its cache
/// assertions still run.
pub fn tree_state(root: &Path) -> Option<String> {
    use std::hash::{DefaultHasher, Hash, Hasher};

    let status = git(root, &["status", "--porcelain"])?;
    let numstat = git(root, &["diff", "HEAD", "--numstat"]).unwrap_or_default();
    let mut hasher = DefaultHasher::new();
    status.hash(&mut hasher);
    numstat.hash(&mut hasher);
    Some(format!("{:016x}", hasher.finish()))
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).current_dir(root).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Directories that are never worth copying or measuring: build output and
/// dependency trees, which are large, generated, and not what poly is being
/// judged on.
const NEVER_COPY: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    ".venv",
    "venv",
    "dist",
    "build",
    ".mypy_cache",
    ".pytest_cache",
    "__pycache__",
];

/// Materialize a disposable copy of `root` to format against.
///
/// Formatting mutates, and the local corpus is the developer's own working
/// trees — so the harness never formats a source tree in place. Even for a
/// throwaway clone this is worth doing: it keeps the lint measurement and the
/// format measurement from seeing each other's writes.
///
/// A copy rather than `git archive` because a root is not necessarily a git
/// repository (one of the local siblings is not), and because the point is to
/// measure the files as they are on disk.
pub fn disposable_copy(root: &Path, into: &Path) -> std::io::Result<()> {
    copy_dir(root, into)
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        if NEVER_COPY.iter().any(|skip| name == *skip) {
            continue;
        }
        // `symlink_metadata`, so a symlinked directory is not followed out of
        // the tree — and is not copied at all, since a link's target may not
        // exist in the copy.
        let meta = entry.metadata()?;
        let target = to.join(&name);
        if meta.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else if meta.is_file() {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}
