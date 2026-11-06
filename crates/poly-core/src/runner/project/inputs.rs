//! What a project-check engine declared in [`Engine::project_inputs`](crate::engine::Engine::project_inputs),
//! resolved against one project: which of its served files are in scope, its side files, and the
//! listing of files it does not serve.
//!
//! The engine states the option keys; this module reads the option values, so no engine name
//! appears here. Everything is handed over as content (a listing entry has none), because a
//! component reads nothing itself and the cache key digests exactly what it is given.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use rayon::prelude::*;

use super::ProjectPlan;
use crate::discover::{DiscoveredFile, list_files};
use crate::engine::{ProjectDirs, ProjectFile};
use crate::filter::is_generated_source;
use crate::language::Language;
use crate::resolve::ConfigSet;

/// The language id of a listing entry: a file the engine does not serve, named without content.
pub const LISTING_LANGUAGE: &str = "listing";

/// The directories an engine's options resolve to.
struct ResolvedDirs {
    /// Relative to the project root; `.` is stripped.
    dirs: Vec<PathBuf>,
    recursive: bool,
    list_unserved: bool,
}

/// A path from configuration that stays inside the project root.
fn inside_root(path: &str) -> Option<PathBuf> {
    let path = Path::new(path);
    let escapes = path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::Prefix(_) | Component::RootDir));
    if escapes {
        return None;
    }
    Some(path.components().filter(|part| *part != Component::CurDir).collect())
}

fn resolve_dirs(declared: &ProjectDirs, options: &toml::Table) -> ResolvedDirs {
    let named: Vec<&str> = declared
        .paths_key
        .as_deref()
        .and_then(|key| options.get(key))
        .and_then(toml::Value::as_array)
        .map(|list| list.iter().filter_map(toml::Value::as_str).collect())
        .unwrap_or_default();
    let named = if named.is_empty() {
        let dir = options
            .get(&declared.dir_key)
            .and_then(toml::Value::as_str)
            .unwrap_or(&declared.dir_default);
        vec![dir]
    } else {
        named
    };
    ResolvedDirs {
        dirs: named
            .into_iter()
            .filter_map(|dir| {
                let resolved = inside_root(dir);
                if resolved.is_none() {
                    tracing::warn!(path = dir, "project check: directory must stay inside the project root");
                }
                resolved
            })
            .collect(),
        recursive: declared
            .recursive_key
            .as_deref()
            .and_then(|key| options.get(key))
            .and_then(toml::Value::as_bool)
            .unwrap_or(false),
        list_unserved: declared.list_unserved,
    }
}

impl ResolvedDirs {
    fn contains(&self, relative: &Path) -> bool {
        let Some(parent) = relative.parent() else {
            return false;
        };
        self.dirs.iter().any(|dir| {
            if self.recursive {
                parent.starts_with(dir)
            } else {
                parent == dir
            }
        })
    }
}

/// The served files `plan`'s engine is in scope for: all of them, or only those under its
/// declared directories, so a Markdown file elsewhere in the repository is never even read.
pub(super) fn in_scope<'a>(plan: &ProjectPlan, root: &Path, files: Vec<&'a DiscoveredFile>) -> Vec<&'a DiscoveredFile> {
    let Some(declared) = plan.engine.project_inputs().dirs else {
        return files;
    };
    let dirs = resolve_dirs(&declared, &plan.config.options);
    files
        .into_iter()
        .filter(|file| dirs.contains(file.path.strip_prefix(root).unwrap_or(&file.path)))
        .collect()
}

/// Read a side file: the path comes from the engine's option (else its default) and must stay
/// inside the project root. It is handed over as [`Language::Other`] named after its file name.
fn side_file(root: &Path, option: Option<&str>, default: &str) -> Option<ProjectFile> {
    let configured = option.unwrap_or(default);
    let Some(relative) = inside_root(configured) else {
        tracing::warn!(
            path = configured,
            "project check: side input must stay inside the project root"
        );
        return None;
    };
    let path = root.join(&relative);
    let content = std::fs::read_to_string(&path).ok()?;
    let name = relative.file_name()?.to_string_lossy().into_owned();
    Some(ProjectFile {
        path,
        language: Language::Other(name),
        content: Arc::from(content),
    })
}

/// The listing of files the engine does not serve, under its declared directories: names only,
/// every file type, skipping what discovery skips (`.gitignore`, pruned directories, excludes) and,
/// unless the run acts on generated sources, generated files.
fn listing(
    dirs: &ResolvedDirs,
    root: &Path,
    configs: &ConfigSet,
    config_id: usize,
    act_on_generated: &[bool],
    known: &[ProjectFile],
) -> Vec<ProjectFile> {
    let mut listed: Vec<PathBuf> = dirs
        .dirs
        .iter()
        .map(|dir| root.join(dir))
        .filter(|dir| dir.is_dir())
        .flat_map(|dir| list_files(&dir, configs, dirs.recursive))
        .filter(|path| !known.iter().any(|file| file.path == *path))
        .collect();
    listed.sort();
    listed.dedup();
    if !act_on_generated[config_id] {
        listed = listed
            .into_par_iter()
            .filter(|path| {
                !std::fs::read(path).is_ok_and(|bytes| is_generated_source(&String::from_utf8_lossy(&bytes)))
            })
            .collect();
    }
    listed
        .into_iter()
        .map(|path| ProjectFile {
            path,
            language: Language::Other(LISTING_LANGUAGE.to_owned()),
            content: Arc::from(""),
        })
        .collect()
}

/// Add what `plan`'s engine declared beyond its served files — its side files and the listing —
/// to `loaded`, then re-sort so the cache key does not depend on where they landed.
pub(super) fn add_declared_inputs(
    plan: &ProjectPlan,
    root: &Path,
    configs: &ConfigSet,
    act_on_generated: &[bool],
    loaded: &mut Vec<ProjectFile>,
) {
    let declared = plan.engine.project_inputs();
    for file in &declared.files {
        let option = plan.config.options.get(&file.option_key).and_then(toml::Value::as_str);
        if let Some(side) = side_file(root, option, &file.default_path) {
            loaded.push(side);
        }
    }
    if let Some(dirs) = declared.dirs.filter(|dirs| dirs.list_unserved) {
        let dirs = resolve_dirs(&dirs, &plan.config.options);
        if dirs.list_unserved {
            let extra = listing(&dirs, root, configs, plan.config_id, act_on_generated, loaded);
            loaded.extend(extra);
        }
    }
    loaded.sort_by(|a, b| a.path.cmp(&b.path));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declared() -> ProjectDirs {
        ProjectDirs {
            dir_key: "dir".to_owned(),
            dir_default: "docs/adr".to_owned(),
            paths_key: Some("paths".to_owned()),
            recursive_key: Some("recursive".to_owned()),
            list_unserved: true,
        }
    }

    fn table(text: &str) -> toml::Table {
        toml::from_str(text).expect("toml")
    }

    #[test]
    fn a_path_leaving_the_root_is_refused() {
        assert_eq!(inside_root("./docs/adr"), Some(PathBuf::from("docs/adr")));
        assert_eq!(inside_root("../x"), None);
        assert_eq!(inside_root("/etc"), None);
        assert_eq!(inside_root("a/../../x"), None);
    }

    #[test]
    fn the_default_directory_applies_and_only_its_direct_children_are_in_scope() {
        let dirs = resolve_dirs(&declared(), &table(""));
        assert!(dirs.contains(Path::new("docs/adr/0001-x.md")));
        assert!(!dirs.contains(Path::new("docs/adr/sub/0001-x.md")));
        assert!(!dirs.contains(Path::new("docs/other/0001-x.md")));
        assert!(!dirs.contains(Path::new("0001-x.md")));
    }

    #[test]
    fn recursive_takes_subdirectories_and_paths_replace_dir() {
        let dirs = resolve_dirs(
            &declared(),
            &table("recursive = true\npaths = [\"a\", \"b/c\"]\ndir = \"ignored\""),
        );
        assert!(dirs.contains(Path::new("a/x/y.md")));
        assert!(dirs.contains(Path::new("b/c/y.md")));
        assert!(!dirs.contains(Path::new("ignored/y.md")));
    }

    #[test]
    fn a_directory_leaving_the_root_leaves_nothing_in_scope() {
        let dirs = resolve_dirs(&declared(), &table("dir = \"../outside\""));
        assert!(!dirs.contains(Path::new("../outside/0001-x.md")));
    }
}
