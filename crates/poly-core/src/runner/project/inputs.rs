//! What a project-check engine declared in [`Engine::project_inputs`](crate::engine::Engine::project_inputs),
//! resolved against one project: the files it serves under its declared directories, its side
//! files, and the listing of the files it does not serve.
//!
//! The engine states the option keys; this module reads the option values, so no engine name
//! appears here. Everything is handed over as content (a listing entry has none), because a
//! component reads nothing itself and the cache key digests exactly what it is given.
//!
//! An engine that declares directories is handed the whole tree under them, walked here, never the
//! run's own file set: what the run was asked to lint decides which findings are reported
//! ([`super::scope`]), not what the engine sees, so a verdict about a log does not change with the
//! paths named on the command line and the cache key is the same either way.

use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use rayon::prelude::*;

use super::{ProjectPlan, RunInputs, load_files};
use crate::discover::{DiscoveredFile, detect, list_files};
use crate::engine::{ProjectDirs, ProjectFile};
use crate::filter::is_generated_source;
use crate::language::Language;
use crate::resolve::ConfigSet;
use poly_engine_api::project::LISTING_LANGUAGE;

/// How much of a listed file is read to tell whether it is generated: the marker sits in the
/// header, so the rest is not worth a read per file in a directory the engine only wants names from.
/// A file whose front matter alone is longer than this reads as hand-written, which only matters for
/// a generated, unserved file that carries a very long front matter block before its marker.
const GENERATED_PREFIX_BYTES: u64 = 16 * 1024;

/// The directories an engine's options resolve to.
#[derive(Debug)]
pub(super) struct ResolvedDirs {
    /// Relative to the project root; `.` is stripped.
    dirs: Vec<PathBuf>,
    recursive: bool,
    list_unserved: bool,
}

/// Where a path stands against the project root.
enum Placement {
    Inside,
    Missing,
    /// A symlink, or a path that resolves outside the root: never followed.
    Unsafe,
}

/// Whether `path` exists and stays inside `root` once symlinks are resolved. A path that is itself
/// a symlink is refused outright, even when its target is inside, so what is read is what the
/// configuration names.
fn check_inside(root: &Path, path: &Path) -> Placement {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return Placement::Missing;
    };
    if metadata.file_type().is_symlink() {
        tracing::warn!(path = %path.display(), "project check: symlinks are not followed");
        return Placement::Unsafe;
    }
    let (Ok(root), Ok(resolved)) = (std::fs::canonicalize(root), std::fs::canonicalize(path)) else {
        return Placement::Missing;
    };
    if resolved.starts_with(&root) {
        Placement::Inside
    } else {
        tracing::warn!(path = %path.display(), "project check: path resolves outside the project root");
        Placement::Unsafe
    }
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

fn typed<'a, T>(
    options: &'a toml::Table,
    key: &str,
    expected: &str,
    read: impl FnOnce(&'a toml::Value) -> Option<T>,
) -> Result<Option<T>, String> {
    match options.get(key) {
        None => Ok(None),
        Some(value) => read(value)
            .map(Some)
            .ok_or_else(|| format!("option `{key}` must be {expected}, found {}", value.type_str())),
    }
}

/// Read the engine's directory options. A key of the wrong type is an error naming it, never a
/// silent fall back to the default directory, which would judge the wrong tree.
pub(super) fn resolve_dirs(declared: &ProjectDirs, options: &toml::Table) -> Result<ResolvedDirs, String> {
    let mut named: Vec<&str> = Vec::new();
    if let Some(key) = declared.paths_key.as_deref() {
        let list = typed(options, key, "an array of strings", |value| value.as_array())?;
        for entry in list.into_iter().flatten() {
            named.push(
                entry
                    .as_str()
                    .ok_or_else(|| format!("option `{key}` must hold only strings, found {}", entry.type_str()))?,
            );
        }
    }
    if named.is_empty() {
        let dir = typed(options, &declared.dir_key, "a string", toml::Value::as_str)?.unwrap_or(&declared.dir_default);
        named.push(dir);
    }
    let recursive = match declared.recursive_key.as_deref() {
        Some(key) => typed(options, key, "a boolean", toml::Value::as_bool)?.unwrap_or(false),
        None => false,
    };
    let mut dirs: Vec<PathBuf> = Vec::new();
    for dir in named {
        dirs.push(inside_root(dir).ok_or_else(|| format!("directory `{dir}` must stay inside the project root"))?);
    }
    dirs.sort();
    dirs.dedup();
    Ok(ResolvedDirs {
        dirs,
        recursive,
        list_unserved: declared.list_unserved,
    })
}

/// The directories `plan`'s engine declared, resolved, or `None` when it declared none.
pub(super) fn plan_dirs(plan: &ProjectPlan) -> Result<Option<ResolvedDirs>, String> {
    plan.engine
        .project_inputs()
        .dirs
        .map(|declared| resolve_dirs(&declared, &plan.config.options))
        .transpose()
}

impl ResolvedDirs {
    /// The absolute directories, for asking whether a run reaches them.
    pub(super) fn absolute(&self, root: &Path) -> impl Iterator<Item = PathBuf> + '_ {
        let root = root.to_path_buf();
        self.dirs.iter().map(move |dir| root.join(dir))
    }
}

/// Read a side file: the path comes from the engine's option (else its default) and must stay
/// inside the project root. It is handed over as [`Language::Other`] named after its file name.
///
/// A file that does not exist is `Ok(None)`: the project has not adopted that input. One that
/// exists but cannot be used (a symlink, outside the root, unreadable, not UTF-8) is an error,
/// because skipping it would silently switch off every check that judges the project against it.
fn side_file(root: &Path, option: Option<&str>, default: &str) -> Result<Option<ProjectFile>, String> {
    let configured = option.unwrap_or(default);
    let relative = inside_root(configured)
        .ok_or_else(|| format!("side input `{configured}` must stay inside the project root"))?;
    let path = root.join(&relative);
    match check_inside(root, &path) {
        Placement::Inside => {}
        Placement::Missing => return Ok(None),
        Placement::Unsafe => {
            return Err(format!(
                "side input `{configured}` is a symlink or resolves outside the project root"
            ));
        }
    }
    let content = std::fs::read_to_string(&path)
        .map_err(|error| format!("side input `{configured}` cannot be read as UTF-8 text: {error}"))?;
    let Some(name) = relative.file_name() else {
        return Err(format!("side input `{configured}` does not name a file"));
    };
    Ok(Some(ProjectFile {
        path,
        language: Language::Other(name.to_string_lossy().into_owned()),
        content: Arc::from(content),
    }))
}

/// Where `plan`'s engine keeps its side files, whether or not they exist.
fn side_paths(plan: &ProjectPlan, root: &Path) -> Vec<PathBuf> {
    plan.engine
        .project_inputs()
        .files
        .iter()
        .filter_map(|file| {
            let option = plan.config.options.get(&file.option_key).and_then(toml::Value::as_str);
            inside_root(option.unwrap_or(&file.default_path))
        })
        .map(|relative| root.join(relative))
        .collect()
}

/// Whether the head of `path` announces a generated file. Only a bounded prefix is read.
fn head_is_generated(path: &Path) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = Vec::new();
    if file.take(GENERATED_PREFIX_BYTES).read_to_end(&mut head).is_err() {
        return false;
    }
    is_generated_source(&String::from_utf8_lossy(&head))
}

/// The files under `dirs`, split by whether `plan`'s engine serves them: the served ones to load
/// with content, the rest to list by name. Both honor what discovery honors (`.gitignore`, pruned
/// directories, `[discovery] exclude` and the run's `--exclude`) and stay in `plan`'s config scope.
fn walk(
    plan: &ProjectPlan,
    root: &Path,
    dirs: &ResolvedDirs,
    configs: &ConfigSet,
    exclude: &[String],
) -> (Vec<DiscoveredFile>, Vec<PathBuf>) {
    let served = plan.engine.languages();
    let mut owned: Vec<DiscoveredFile> = Vec::new();
    let mut unserved: Vec<PathBuf> = Vec::new();
    for dir in dirs.absolute(root) {
        match check_inside(root, &dir) {
            Placement::Inside if dir.is_dir() => {}
            _ => continue,
        }
        for path in list_files(&dir, configs, dirs.recursive, exclude) {
            if configs.config_id_for(&path) != plan.config_id {
                continue;
            }
            match detect(&path) {
                Some(language) if served.is_empty() || served.contains(&language) => owned.push(DiscoveredFile {
                    path,
                    language,
                    config_id: plan.config_id,
                }),
                _ => unserved.push(path),
            }
        }
    }
    owned.sort_by(|a, b| a.path.cmp(&b.path));
    owned.dedup_by(|a, b| a.path == b.path);
    unserved.sort();
    unserved.dedup();
    (owned, unserved)
}

/// The name-only entries for `unserved`: not a side file of the engine, and unless the run acts on
/// generated sources, not generated.
fn listing(unserved: Vec<PathBuf>, sides: &[PathBuf], act_on_generated: bool) -> Vec<ProjectFile> {
    let kept: Vec<PathBuf> = unserved
        .into_par_iter()
        .filter(|path| !sides.contains(path))
        .filter(|path| act_on_generated || !head_is_generated(path))
        .collect();
    kept.into_iter()
        .map(|path| ProjectFile {
            path,
            language: Language::Other(LISTING_LANGUAGE.to_owned()),
            content: Arc::from(""),
        })
        .collect()
}

/// Everything `plan`'s engine is handed: its served files (the run's own when it declared no
/// directories, else the tree under them), its side files and the listing, sorted by path so the
/// cache key does not depend on where anything landed.
pub(super) fn load(
    plan: &ProjectPlan,
    root: &Path,
    dirs: Option<&ResolvedDirs>,
    scoped: &[&DiscoveredFile],
    run: &RunInputs<'_>,
) -> Result<Vec<ProjectFile>, String> {
    let (mut loaded, unserved) = match dirs {
        Some(dirs) => {
            let (owned, unserved) = walk(plan, root, dirs, run.configs, run.exclude);
            let refs: Vec<&DiscoveredFile> = owned.iter().collect();
            (load_files(&refs, run.act_on_generated), unserved)
        }
        None => (load_files(scoped, run.act_on_generated), Vec::new()),
    };
    let sides = side_paths(plan, root);
    for file in &plan.engine.project_inputs().files {
        let option = plan.config.options.get(&file.option_key).and_then(toml::Value::as_str);
        loaded.extend(side_file(root, option, &file.default_path)?);
    }
    if dirs.is_some_and(|dirs| dirs.list_unserved) {
        loaded.extend(listing(unserved, &sides, run.act_on_generated[plan.config_id]));
    }
    loaded.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(loaded)
}

/// Whether `loaded` holds anything but listing entries: a served file or a side file. A listing
/// alone is names with nothing to judge them against.
pub(super) fn has_substance(loaded: &[ProjectFile]) -> bool {
    loaded
        .iter()
        .any(|file| !matches!(&file.language, Language::Other(name) if name == LISTING_LANGUAGE))
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

    fn dirs_of(text: &str) -> Vec<PathBuf> {
        resolve_dirs(&declared(), &table(text)).expect("resolves").dirs
    }

    #[test]
    fn a_path_leaving_the_root_is_refused() {
        assert_eq!(inside_root("./docs/adr"), Some(PathBuf::from("docs/adr")));
        assert_eq!(inside_root("../x"), None);
        assert_eq!(inside_root("/etc"), None);
        assert_eq!(inside_root("a/../../x"), None);
    }

    #[test]
    fn the_default_directory_applies_when_nothing_is_set() {
        assert_eq!(dirs_of(""), [PathBuf::from("docs/adr")]);
    }

    #[test]
    fn paths_replace_dir_and_recursive_is_read() {
        let resolved = resolve_dirs(
            &declared(),
            &table("recursive = true\npaths = [\"b/c\", \"a\", \"./a\"]\ndir = \"ignored\""),
        )
        .expect("resolves");
        assert_eq!(resolved.dirs, [PathBuf::from("a"), PathBuf::from("b/c")]);
        assert!(resolved.recursive);
    }

    #[test]
    fn a_directory_leaving_the_root_is_an_error_naming_it() {
        for text in ["dir = \"../outside\"", "dir = \"/etc\"", "paths = [\"ok\", \"../x\"]"] {
            let error = resolve_dirs(&declared(), &table(text)).expect_err(text);
            assert!(error.contains("inside the project root"), "{text}: {error}");
        }
    }

    #[test]
    fn an_option_of_the_wrong_type_is_an_error_naming_it() {
        for (text, key) in [
            ("dir = 3", "dir"),
            ("paths = \"docs\"", "paths"),
            ("paths = [\"a\", 1]", "paths"),
            ("recursive = \"yes\"", "recursive"),
        ] {
            let error = resolve_dirs(&declared(), &table(text)).expect_err(text);
            assert!(error.contains(&format!("`{key}`")), "{text}: {error}");
        }
    }

    #[test]
    fn the_side_file_reads_inside_the_root_and_ignores_the_dot_prefix() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.lock"), "locked").unwrap();
        let side = side_file(root.path(), Some("./a.lock"), "default.lock")
            .expect("usable")
            .expect("read");
        assert_eq!(side.path, root.path().join("a.lock"));
        assert_eq!(side.language, Language::Other("a.lock".to_owned()));
        let default = side_file(root.path(), None, "a.lock")
            .expect("usable")
            .expect("default");
        assert_eq!(default.content.as_ref(), "locked");
    }

    #[test]
    fn a_missing_side_file_is_not_an_error_but_an_escaping_one_is() {
        let root = tempfile::tempdir().unwrap();
        assert!(
            side_file(root.path(), None, "missing.lock")
                .expect("absent is fine")
                .is_none()
        );
        let error = side_file(root.path(), Some("../a.lock"), "default.lock").expect_err("escapes");
        assert!(error.contains("inside the project root"), "{error}");
    }

    #[test]
    fn a_side_file_that_is_not_utf8_is_an_error() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.lock"), [0xff, 0xfe, 0x00]).unwrap();
        let error = side_file(root.path(), None, "a.lock").expect_err("not text");
        assert!(error.contains("a.lock"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_side_file_is_an_error() {
        let outside = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "x").unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret"), root.path().join("a.lock")).unwrap();
        let error = side_file(root.path(), None, "a.lock").expect_err("symlink");
        assert!(error.contains("symlink"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_directory_is_refused_and_a_plain_one_is_not() {
        let outside = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("real")).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("link")).unwrap();
        assert!(matches!(
            check_inside(root.path(), &root.path().join("real")),
            Placement::Inside
        ));
        assert!(matches!(
            check_inside(root.path(), &root.path().join("link")),
            Placement::Unsafe
        ));
        assert!(matches!(
            check_inside(root.path(), &root.path().join("nope")),
            Placement::Missing
        ));
    }

    #[test]
    fn a_listing_skips_side_files_and_generated_files_and_reads_only_a_prefix() {
        let root = tempfile::tempdir().unwrap();
        let write = |name: &str, text: &str| {
            let path = root.path().join(name);
            std::fs::write(&path, text).unwrap();
            path
        };
        let plain = write("notes.txt", "scratch\n");
        let generated = write("gen.txt", "// Code generated by tool. DO NOT EDIT.\n");
        let lock = write("adrlint.lock", "x");
        let late = write(
            "late.txt",
            &format!("{}\n// Code generated by tool. DO NOT EDIT.\n", "x".repeat(20_000)),
        );
        let paths = vec![plain.clone(), generated.clone(), lock.clone(), late.clone()];
        let names = |listed: Vec<ProjectFile>| -> Vec<PathBuf> { listed.into_iter().map(|f| f.path).collect() };

        let mut kept = names(listing(paths.clone(), std::slice::from_ref(&lock), false));
        kept.sort();
        assert_eq!(kept, [late.clone(), plain.clone()], "generated and side files drop out");
        let everything = names(listing(paths, std::slice::from_ref(&lock), true));
        assert_eq!(everything.len(), 3, "a run acting on generated files lists them");
        assert!(listing(vec![plain], &[], false).iter().all(|f| f.content.is_empty()));
    }

    #[test]
    fn only_served_files_and_side_files_count_as_substance() {
        let entry = |language| ProjectFile {
            path: PathBuf::from("x"),
            language,
            content: Arc::from(""),
        };
        let listed = entry(Language::Other(LISTING_LANGUAGE.to_owned()));
        assert!(!has_substance(&[]));
        assert!(!has_substance(std::slice::from_ref(&listed)));
        assert!(has_substance(&[listed.clone(), entry(Language::Markdown)]));
        assert!(has_substance(&[listed, entry(Language::Other("a.lock".to_owned()))]));
    }
}
