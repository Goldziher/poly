//! File discovery: walk the given paths respecting `.gitignore`, and tag each
//! file with its detected [`Language`].

use std::path::PathBuf;

use ignore::WalkBuilder;

use crate::language::Language;

/// A file found during discovery, tagged with its detected language.
#[derive(Debug, Clone)]
pub struct DiscoveredFile {
    /// Path to the file.
    pub path: PathBuf,
    /// Detected language.
    pub language: Language,
}

/// Recursively discover supported files under `paths`.
pub fn discover(paths: &[PathBuf]) -> Vec<DiscoveredFile> {
    let mut out = Vec::new();
    for root in paths {
        let walker = WalkBuilder::new(root)
            .hidden(false)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .parents(true)
            .build();
        for entry in walker.flatten() {
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            let path = entry.path();
            if let Some(language) = Language::from_path(path) {
                out.push(DiscoveredFile {
                    path: path.to_path_buf(),
                    language,
                });
            }
        }
    }
    out
}
