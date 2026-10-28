//! Native `oxlint` configuration discovery.
//!
//! Finds the nearest `.oxlintrc.json` / `.oxlintrc.jsonc`. Parsing is left to `poly-core`, which
//! reads the file with `oxc_linter`'s own `Oxlintrc` so rule names, categories, plugins and
//! overrides mean exactly what they mean to oxlint.

use std::path::{Path, PathBuf};

/// Native oxlint config file names in preference order.
const OXLINT_FILE_NAMES: [&str; 2] = [".oxlintrc.json", ".oxlintrc.jsonc"];

/// Where the project's own oxlint configuration lives, if it has one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OxlintNative {
    /// The root config file, when one was found.
    pub file: Option<PathBuf>,
}

/// Resolve the oxlint config governing `dir`: the nearest file walking upward, bounded at the
/// enclosing git root.
pub(crate) fn resolve_oxlint_native(dir: &Path) -> OxlintNative {
    let dir = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    let mut current = Some(dir.as_path());
    while let Some(candidate) = current {
        for name in OXLINT_FILE_NAMES {
            let path = candidate.join(name);
            if path.is_file() {
                return OxlintNative { file: Some(path) };
            }
        }
        if candidate.join(".git").exists() {
            break;
        }
        current = candidate.parent();
    }
    OxlintNative::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_nearest_config_without_crossing_the_git_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".oxlintrc.json"), "{}").unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("pkg")).unwrap();
        assert_eq!(resolve_oxlint_native(&repo.join("pkg")), OxlintNative::default());
        std::fs::write(repo.join(".oxlintrc.jsonc"), "{}").unwrap();
        assert_eq!(
            resolve_oxlint_native(&repo.join("pkg")).file,
            Some(repo.join(".oxlintrc.jsonc"))
        );
    }
}
