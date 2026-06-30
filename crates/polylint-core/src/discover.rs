//! File discovery: walk the given paths respecting `.gitignore`, and tag each
//! file with its detected [`Language`].

use std::path::PathBuf;

use ignore::WalkBuilder;
use ignore::overrides::OverrideBuilder;

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
///
/// `exclude` holds gitignore-style globs from `[discovery] exclude`; matching
/// paths are pruned from the walk in addition to `.gitignore` and the built-in
/// [`PRUNED_DIRECTORIES`] set. The globs are matched relative to each walk root,
/// so an explicitly passed path argument is never affected by another root's
/// excludes.
pub fn discover(paths: &[PathBuf], exclude: &[String]) -> Vec<DiscoveredFile> {
    let mut out = Vec::new();
    for root in paths {
        let mut builder = WalkBuilder::new(root);
        builder
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
            });
        if let Some(overrides) = build_excludes(root, exclude) {
            builder.overrides(overrides);
        }
        let walker = builder.build();
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

/// Build an [`ignore::overrides::Override`] from `[discovery] exclude` globs,
/// rooted at `root`. Each glob is added with a leading `!` so it acts as an
/// ignore (exclusion) — with no whitelist glob present, the override behaves
/// like a `.gitignore`: matched paths are pruned and everything else is kept.
/// Returns `None` when there is nothing to exclude. An individual malformed
/// glob is skipped with a warning rather than aborting discovery.
fn build_excludes(
    root: &std::path::Path,
    exclude: &[String],
) -> Option<ignore::overrides::Override> {
    if exclude.is_empty() {
        return None;
    }
    let mut builder = OverrideBuilder::new(root);
    for glob in exclude {
        if let Err(error) = builder.add(&format!("!{glob}")) {
            tracing::warn!(%glob, %error, "skipping invalid [discovery] exclude glob");
            continue;
        }
        // `dir/**` matches files *inside* `dir` but not `dir` itself, so the
        // walker would descend the whole subtree before discarding each entry.
        // Also exclude the bare directory so it is pruned before descent (like
        // PRUNED_DIRECTORIES), turning an `O(subtree)` walk into `O(1)`.
        if let Some(dir) = glob.strip_suffix("/**")
            && let Err(error) = builder.add(&format!("!{dir}"))
        {
            tracing::warn!(%dir, %error, "skipping derived directory exclude");
        }
    }
    match builder.build() {
        Ok(overrides) => Some(overrides),
        Err(error) => {
            tracing::warn!(%error, "failed to build [discovery] exclude globs; ignoring them");
            None
        }
    }
}
