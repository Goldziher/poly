//! Path arithmetic for recipe tools: where a file sits relative to the working
//! directory, which project root governs it, and how a path a tool printed maps
//! back to a file of the run.
//!
//! All of it is lexical (`.` and `..` folded without touching the filesystem)
//! except [`canonical`], because tools disagree on how they print a path: some
//! echo the argument, some print the resolved absolute path (and on macOS
//! `/tmp` resolves to `/private/tmp`).

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Fold `.` and `..` components without consulting the filesystem.
pub(super) fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `path` as an absolute, lexically normalized path under `cwd`.
pub(super) fn absolute(cwd: &Path, path: &Path) -> PathBuf {
    normalize(&cwd.join(path))
}

/// `path` relative to `cwd`, lexically normalized: `./a/b` and `<cwd>/a/b` both
/// become `a/b`. `None` for a path outside `cwd` (or one that climbs out of it).
pub(super) fn relative(cwd: &Path, path: &Path) -> Option<PathBuf> {
    let normalized = normalize(path);
    let rel = if normalized.is_absolute() {
        let real = canonical(cwd);
        let resolved = canonical(&normalized);
        normalized
            .strip_prefix(cwd)
            .or_else(|_| normalized.strip_prefix(&real))
            .or_else(|_| resolved.strip_prefix(&real))
            .ok()?
            .to_path_buf()
    } else {
        normalized
    };
    (!rel.starts_with("..")).then_some(rel)
}

/// The path resolved through symlinks, or the normalized path when it cannot be
/// resolved (a file that does not exist on disk).
pub(super) fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| normalize(path))
}

/// Forward-slash rendering of a relative path, the form every tool argv and
/// every report uses regardless of platform.
pub(super) fn slashed(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// The nearest ancestor directory of `file` (searching from its own directory)
/// holding any of the `markers`, stopping after the repository root (a directory
/// containing `.git`). Memoized: a monorepo run asks the same directories
/// thousands of times.
pub(super) fn project_root(file: &Path, markers: &[String]) -> Option<PathBuf> {
    type Memo = Mutex<HashMap<(PathBuf, Vec<String>), Option<PathBuf>>>;
    static MEMO: OnceLock<Memo> = OnceLock::new();
    let start = file.parent()?.to_path_buf();
    let memo = MEMO.get_or_init(Default::default);
    let key = (start.clone(), markers.to_vec());
    if let Some(known) = memo.lock().expect("root memo poisoned").get(&key) {
        return known.clone();
    }
    let mut found = None;
    for directory in start.ancestors() {
        if markers.iter().any(|marker| directory.join(marker).is_file()) {
            found = Some(directory.to_path_buf());
            break;
        }
        if directory.join(".git").exists() {
            break;
        }
    }
    memo.lock().expect("root memo poisoned").insert(key, found.clone());
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_folds_dots_without_the_filesystem() {
        assert_eq!(normalize(Path::new("./a/./b/../c")), PathBuf::from("a/c"));
        assert_eq!(normalize(Path::new("../a")), PathBuf::from("../a"));
    }

    #[test]
    fn project_root_is_the_nearest_marker_directory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let chart = tmp.path().join("charts/web");
        std::fs::create_dir_all(chart.join("templates")).expect("mkdir");
        std::fs::write(chart.join("Chart.yaml"), "name: web\n").expect("write");
        std::fs::create_dir(tmp.path().join(".git")).expect("git");
        let marker = ["Chart.yaml".to_owned()];
        assert_eq!(
            project_root(&chart.join("templates/a.yaml"), &marker),
            Some(chart.clone())
        );
        assert_eq!(project_root(&tmp.path().join("README.md"), &marker), None);
    }

    #[test]
    fn project_root_stops_at_the_repository_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("go.mod"), "module x\n").expect("write");
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).expect("git");
        std::fs::create_dir_all(repo.join("pkg")).expect("pkg");
        let marker = ["go.mod".to_owned()];
        assert_eq!(project_root(&repo.join("pkg/a.go"), &marker), None);
    }
}
