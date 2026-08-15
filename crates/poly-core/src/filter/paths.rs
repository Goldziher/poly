//! Path-based filtering: resolving a discovered file to the repo-relative path
//! that `[per-file-ignores]` globs match against, and recognizing generated lock
//! files that `poly fmt` never rewrites on a directory walk.

use std::path::{Path, PathBuf};

/// File path relative to the run root, forward-slash normalized, for matching
/// repo-rooted `[per-file-ignores]` globs. Strips the first of `bases` (cwd plus
/// the explicitly passed roots) that prefixes the path, so both `poly lint .`
/// (relative paths) and `poly lint /abs/repo` (absolute paths) resolve to a
/// repo-relative path the globs can match.
pub(crate) fn relative_for_match(path: &Path, bases: &[PathBuf]) -> String {
    let mut rel = path;
    for base in bases {
        if let Ok(stripped) = path.strip_prefix(base) {
            if stripped.as_os_str().is_empty() {
                continue;
            }
            rel = stripped;
            break;
        }
    }
    let rel = rel.strip_prefix(".").unwrap_or(rel);
    let text = rel.to_string_lossy();
    if text.contains('\\') {
        text.replace('\\', "/")
    } else {
        text.into_owned()
    }
}

/// Prefix bases for [`relative_for_match`]: the working directory (when
/// available) followed by the explicitly passed roots, so per-file-ignore globs
/// resolve against whichever one prefixes a discovered file.
pub(crate) fn match_bases(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut bases = Vec::with_capacity(paths.len() + 1);
    match std::env::current_dir() {
        Ok(cwd) => bases.push(cwd),
        Err(error) => {
            tracing::warn!(%error, "cannot determine working directory; \
                 per-file-ignores fall back to matching against the passed paths");
        }
    }
    bases.extend(paths.iter().cloned());
    bases
}

/// Generated lock files, by exact name, that `poly fmt` never rewrites on a
/// directory walk. Any `*.lock` file is also treated as a lock file; these are
/// the ones whose names do not end in `.lock`.
const LOCKFILE_NAMES: &[&str] = &[
    "package-lock.json",
    "npm-shrinkwrap.json",
    "pnpm-lock.yaml",
    "bun.lockb",
];

/// Whether `path` is a machine-generated lock file that must not be reformatted.
/// Matched by the `*.lock` extension (Cargo.lock, yarn.lock, poetry.lock,
/// uv.lock, composer.lock, Gemfile.lock, flake.lock, deno.lock, …) or by an
/// exact name in [`LOCKFILE_NAMES`] for the lock files that don't end in `.lock`.
pub(crate) fn is_generated_lockfile(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    name.ends_with(".lock") || LOCKFILE_NAMES.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_for_match_strips_cwd_and_passed_roots() {
        let cwd = PathBuf::from("/work/repo");
        assert_eq!(
            relative_for_match(Path::new("/work/repo/tests/a.py"), std::slice::from_ref(&cwd)),
            "tests/a.py"
        );
        let bases = vec![cwd, PathBuf::from("/other/root")];
        assert_eq!(
            relative_for_match(Path::new("/other/root/tests/a.py"), &bases),
            "tests/a.py"
        );
        assert_eq!(
            relative_for_match(Path::new("./tests/a.py"), &[PathBuf::from("/x")]),
            "tests/a.py"
        );
        let file = PathBuf::from("tests/a.py");
        assert_eq!(
            relative_for_match(Path::new("tests/a.py"), &[PathBuf::from("/cwd"), file]),
            "tests/a.py"
        );
    }

    #[test]
    fn recognizes_generated_lock_files() {
        for name in [
            "Cargo.lock",
            "yarn.lock",
            "poetry.lock",
            "uv.lock",
            "Gemfile.lock",
            "flake.lock",
            "composer.lock",
            "package-lock.json",
            "pnpm-lock.yaml",
            "npm-shrinkwrap.json",
            "bun.lockb",
        ] {
            assert!(
                is_generated_lockfile(Path::new(name)),
                "{name} should be treated as a lock file"
            );
        }
        for name in ["main.rs", "Cargo.toml", "package.json", "lockfile.txt"] {
            assert!(
                !is_generated_lockfile(Path::new(name)),
                "{name} must not be treated as a lock file"
            );
        }
    }
}
