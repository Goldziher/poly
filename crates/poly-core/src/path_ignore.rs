//! Gitignore-style path exclusion keyed by the directory each pattern list is relative to.

use std::path::{Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};

/// Pattern lists from one or more config files, each relative to its own directory.
#[derive(Debug)]
pub(crate) struct PathIgnore {
    matchers: Vec<(PathBuf, Gitignore)>,
}

impl PathIgnore {
    /// Compile `(directory, patterns)` pairs. `None` when there is nothing to match.
    pub(crate) fn new(sets: &[(PathBuf, Vec<String>)]) -> Option<PathIgnore> {
        let matchers: Vec<(PathBuf, Gitignore)> = sets
            .iter()
            .filter(|(_, patterns)| !patterns.is_empty())
            .filter_map(|(dir, patterns)| {
                let mut builder = GitignoreBuilder::new(dir);
                for pattern in patterns {
                    if builder.add_line(None, pattern).is_err() {
                        tracing::warn!(
                            "ignore pattern `{pattern}` under {} is not a valid glob and is skipped",
                            dir.display()
                        );
                    }
                }
                Some((dir.clone(), builder.build().ok()?))
            })
            .collect();
        (!matchers.is_empty()).then_some(PathIgnore { matchers })
    }

    /// Whether any pattern list excludes `path`.
    pub(crate) fn is_ignored(&self, path: &Path) -> bool {
        let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        self.matchers.iter().any(|(dir, matcher)| {
            absolute
                .strip_prefix(dir)
                .is_ok_and(|relative| matcher.matched_path_or_any_parents(relative, false).is_ignore())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_list_applies_only_beneath_its_own_directory() {
        let ignore = PathIgnore::new(&[
            (PathBuf::from("/repo"), vec!["**/dist/**".to_string()]),
            (PathBuf::from("/repo/pkg"), vec!["generated.ts".to_string()]),
        ])
        .unwrap();
        assert!(ignore.is_ignored(Path::new("/repo/a/dist/x.js")));
        assert!(ignore.is_ignored(Path::new("/repo/pkg/generated.ts")));
        assert!(!ignore.is_ignored(Path::new("/repo/other/generated.ts")));
    }

    #[test]
    fn empty_lists_compile_to_nothing() {
        assert!(PathIgnore::new(&[(PathBuf::from("/repo"), Vec::new())]).is_none());
    }
}
