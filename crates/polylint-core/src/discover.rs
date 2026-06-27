//! File discovery: walk the given paths respecting `.gitignore`, and tag each
//! file with its detected [`Language`].

use std::path::PathBuf;

use ignore::WalkBuilder;

use crate::language::Language;

/// Directory names pruned from every walk regardless of git-tracking.
///
/// These hold vendored third-party code (`node_modules`, `vendor`, `deps`),
/// build artifacts (`target`, `dist`, `build`, `.next`, `.nuxt`, `.gradle`),
/// or tool caches/environments (`.venv`, `venv`, `__pycache__`, `.mypy_cache`,
/// `.ruff_cache`, `.pytest_cache`, `.tox`, `coverage`, `.polylint`, `.git`).
/// None of it is source the user authored, so no linter or formatter should
/// touch it — and these directories are frequently *tracked* in git (e.g.
/// committed Hex `deps/` CHANGELOGs), so `.gitignore` alone does not exclude
/// them. Pruning them at the walk boundary also avoids descending into large
/// generated subtrees.
const PRUNED_DIRECTORIES: &[&str] = &[
    "node_modules",
    "vendor",
    "deps",
    "target",
    "dist",
    "build",
    ".git",
    ".venv",
    "venv",
    "__pycache__",
    ".mypy_cache",
    ".ruff_cache",
    ".pytest_cache",
    ".tox",
    ".gradle",
    ".next",
    ".nuxt",
    "coverage",
    ".polylint",
];

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
            .filter_entry(|entry| {
                // Prune known vendored/generated directories, but only when they
                // are nested (depth > 0) and are directories — so an explicitly
                // passed root such as `node_modules/foo.js` is still discovered,
                // and a plain file that happens to share one of these names is
                // never dropped.
                let is_directory = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                if entry.depth() > 0 && is_directory {
                    let name = entry.file_name();
                    return !PRUNED_DIRECTORIES.iter().any(|pruned| name == *pruned);
                }
                true
            })
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
