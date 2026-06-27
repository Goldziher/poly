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

/// Detect a file's language: tier-1 extension mapping first, then the
/// tree-sitter language pack's path detection for the long tail (mapped to
/// [`Language::Other`] so the generic tier handles it). Files neither can
/// identify are skipped.
fn detect(path: &std::path::Path) -> Option<Language> {
    if let Some(language) = Language::from_path(path) {
        return Some(language);
    }
    let name = tree_sitter_language_pack::detect_language(&path.to_string_lossy())?;
    Some(Language::Other(name.to_string()))
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
            if let Some(language) = detect(path) {
                out.push(DiscoveredFile {
                    path: path.to_path_buf(),
                    language,
                });
            }
        }
    }
    out
}
