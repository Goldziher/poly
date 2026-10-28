//! `[hooks]` tables in a `poly.toml` below the repository root.
//!
//! Git runs every hook from the repository root, where poly reads only that
//! root's `poly.toml`. A nested `poly.toml` that declares `[hooks]` is therefore
//! picked up here: its hooks are lowered like the root's, then confined to its own
//! directory.
//!
//! - they apply only to files under that directory, so a nested config with no
//!   staged file under it contributes nothing;
//! - they run with that directory as their working directory and receive file
//!   arguments relative to it, as a `root = "<dir>"` job does;
//! - their `files` / `exclude` globs are relative to that directory;
//! - their ids are prefixed with the directory (`svc:ruff-format`).
//!
//! Only the nested file's own `[hooks]` table applies. The root's hooks are not
//! inherited, and a nested table's `isolate`, `snapshot`, `sources` and stage-level
//! `precondition` / `before` / `after` are not honoured: those belong to the run as
//! a whole.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use poly_config::{HookCacheMode, PolyConfig};
use poly_hooks::model::StageSpec;

use super::lower;

/// The file a nested hook table lives in.
const CONFIG_NAME: &str = "poly.toml";

/// A `poly.toml` below the repository root that declares its own `[hooks]`.
pub struct NestedHooks {
    /// The directory holding the `poly.toml`, relative to the repository root.
    pub dir: PathBuf,
    /// The nested file's own configuration (not cascaded over the root's).
    pub config: PolyConfig,
}

/// What a nested hook table is lowered against.
pub struct NestedRun<'a> {
    /// The running `poly` binary, the entry for builtins.
    pub poly_bin: &'a Path,
    /// The stage being run.
    pub stage: poly_hooks::Stage,
    /// The candidate files, relative to the repository root.
    pub files: &'a [PathBuf],
    /// The arguments git passed to the hook.
    pub git_args: &'a [String],
    /// The global `[cache.results] hooks` mode.
    pub cache_mode: &'a HookCacheMode,
    /// The repository root.
    pub root: &'a Path,
}

/// The directories (relative to `root`, outermost first) between a file in
/// `files` and the repository root that hold a `poly.toml`.
fn config_dirs(root: &Path, files: &[PathBuf]) -> Vec<PathBuf> {
    let mut seen: BTreeMap<PathBuf, bool> = BTreeMap::new();
    for file in files {
        let mut dir = file.parent();
        while let Some(current) = dir.filter(|dir| !dir.as_os_str().is_empty()) {
            if seen.contains_key(current) {
                break;
            }
            seen.insert(current.to_path_buf(), root.join(current).join(CONFIG_NAME).is_file());
            dir = current.parent();
        }
    }
    seen.into_iter()
        .filter(|(_, has_config)| *has_config)
        .map(|(dir, _)| dir)
        .collect()
}

/// Load every nested `poly.toml` above a file in `files` that declares `[hooks]`.
///
/// # Errors
///
/// Returns `Err` when a nested config cannot be read or parsed: a broken table
/// must stop the commit rather than silently skip its hooks.
pub fn discover(root: &Path, files: &[PathBuf]) -> Result<Vec<NestedHooks>> {
    let dirs = config_dirs(root, files);
    if dirs.is_empty() {
        return Ok(Vec::new());
    }
    let resolver = crate::config_sources::RemoteExtendsResolver::new(root)?;
    let mut found = Vec::new();
    for dir in dirs {
        let path = root.join(&dir).join(CONFIG_NAME);
        let config = PolyConfig::load_file_with(&path, &resolver)
            .with_context(|| format!("failed to load the nested config {}", path.display()))?;
        if config.hooks.present {
            found.push(NestedHooks { dir, config });
        }
    }
    Ok(found)
}

/// Every nested `poly.toml` that declares `[hooks]` among the tracked files, for
/// `poly hooks install` to learn which git hooks they bind.
///
/// # Errors
///
/// Returns `Err` when the tracked files cannot be listed or a nested config is
/// broken.
pub fn discover_tracked(root: &Path) -> Result<Vec<NestedHooks>> {
    let tracked = poly_hooks::git::list_files(root).context("failed to list the tracked files")?;
    let configs: Vec<PathBuf> = tracked
        .into_iter()
        .filter(|path| path.file_name().is_some_and(|name| name == CONFIG_NAME))
        .collect();
    discover(root, &configs)
}

/// Lower each nested table for the run and add its hooks to `spec`, confined to
/// its directory. Hooks stay ordered by priority.
///
/// # Errors
///
/// Returns `Err` when a nested table does not lower (an invalid glob, an unknown
/// stage name).
pub fn merge_stage(spec: &mut StageSpec, nested: &[NestedHooks], run: &NestedRun<'_>) -> Result<()> {
    for entry in nested {
        let scoped: Vec<PathBuf> = run
            .files
            .iter()
            .filter_map(|file| file.strip_prefix(&entry.dir).ok().map(Path::to_path_buf))
            .collect();
        if scoped.is_empty() {
            continue;
        }
        let mut lowered = lower::lower_stage_with_git_args(
            &entry.config.hooks,
            run.poly_bin,
            run.stage,
            &scoped,
            run.git_args,
            run.cache_mode,
            &run.root.join(&entry.dir),
            &entry.config.tools,
        )
        .with_context(|| format!("failed to lower the hooks of {}", entry.dir.join(CONFIG_NAME).display()))?;
        if lowered.precondition.is_some() || !lowered.before.is_empty() || !lowered.after.is_empty() {
            eprintln!(
                "poly hooks: {}: a nested poly.toml's stage-level precondition/before/after are not run",
                entry.dir.join(CONFIG_NAME).display()
            );
        }
        for hook in &mut lowered.hooks {
            confine(hook, &entry.dir);
        }
        spec.hooks.append(&mut lowered.hooks);
    }
    spec.hooks.sort_by_key(|hook| hook.priority);
    Ok(())
}

/// Make `hook` (lowered against files relative to `dir`) run in `dir` over the
/// repository-relative paths it will be handed.
fn confine(hook: &mut poly_hooks::model::Hook, dir: &Path) {
    hook.id = format!("{}:{}", dir.display(), hook.id);
    hook.cwd = Some(match hook.cwd.take() {
        Some(own) => dir.join(own),
        None => dir.to_path_buf(),
    });
    hook.files = hook.files.take().map(|pattern| pattern.rebased(dir));
    hook.exclude = hook.exclude.take().map(|pattern| pattern.rebased(dir));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(files: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for file in files {
            let path = dir.path().join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        dir
    }

    fn paths(files: &[&str]) -> Vec<PathBuf> {
        files.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn config_dirs_are_the_ancestors_of_a_file_that_hold_a_poly_toml() {
        let repo = tree(&[
            "poly.toml",
            "svc/poly.toml",
            "svc/a/b.py",
            "svc/a/deep/c.py",
            "other/d.py",
        ]);
        let dirs = config_dirs(
            repo.path(),
            &paths(&["svc/a/b.py", "svc/a/deep/c.py", "other/d.py", "top.txt"]),
        );
        assert_eq!(dirs, [PathBuf::from("svc")], "the root's own poly.toml is not nested");
    }

    #[test]
    fn nested_configs_are_found_at_every_level() {
        let repo = tree(&["svc/poly.toml", "svc/inner/poly.toml", "svc/inner/x.py"]);
        let dirs = config_dirs(repo.path(), &paths(&["svc/inner/x.py"]));
        assert_eq!(dirs, [PathBuf::from("svc"), PathBuf::from("svc/inner")]);
    }

    #[test]
    fn a_file_outside_every_nested_dir_finds_none() {
        let repo = tree(&["svc/poly.toml", "other/x.py"]);
        assert!(config_dirs(repo.path(), &paths(&["other/x.py"])).is_empty());
    }

    #[test]
    fn confine_moves_a_hook_into_its_directory_and_rebases_its_globs() {
        let mut hook = poly_hooks::model::Hook::run("fmt", "poly fmt");
        hook.files = Some(poly_hooks::filter::FilePattern::glob(vec!["src/**".to_string()]).unwrap());
        hook.cwd = Some(PathBuf::from("inner"));
        confine(&mut hook, Path::new("svc"));
        assert_eq!(hook.id, "svc:fmt");
        assert_eq!(hook.cwd, Some(PathBuf::from("svc/inner")));
        let files = hook.files.unwrap();
        assert!(files.is_match(Path::new("svc/src/a.py")));
        assert!(!files.is_match(Path::new("src/a.py")));
    }
}
