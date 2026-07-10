//! Hierarchical, monorepo-aware config resolution (ADR 0018).
//!
//! A run may span several projects, each with its own `poly.toml`. This module
//! discovers every in-tree config, resolves each via the cascade in
//! [`poly_config::PolyConfig::resolve_for_dir`] (nearest config wins, inheriting
//! from ancestors up to the workspace root), and maps each discovered file to
//! the nearest config that governs it.
//!
//! The run's root config (loaded by the caller) is always `configs[0]`; nested
//! configs found under the walked paths get subsequent ids. A repo with a single
//! root `poly.toml` and no nested configs resolves every file to `configs[0]` —
//! byte-for-byte the pre-hierarchical behavior.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

use crate::config::Config;
use crate::discover::keep_walk_entry;

/// The resolved configs in effect for a run, plus the directory→config map used
/// to associate each file with the nearest config that governs it.
pub struct ConfigSet {
    /// Deduped resolved configs. `configs[0]` is the run's root config (loaded by
    /// the caller) and the fallback for any file not under a nested config.
    configs: Vec<Config>,
    /// The directory owning each config (parallel to `configs`). `None` only for
    /// the single-config (`--config`) bypass, which has no backing directory.
    dirs: Vec<Option<PathBuf>>,
    /// `(config_dir, config_id)` for every directory-backed config, sorted by
    /// path depth descending so the first ancestor match is the nearest config.
    lookup: Vec<(PathBuf, usize)>,
}

impl ConfigSet {
    /// A single config applied to every file — the `--config <path>` bypass (and
    /// the conformance harness). No nested resolution.
    pub fn single(config: Config) -> Self {
        Self {
            configs: vec![config],
            dirs: vec![None],
            lookup: Vec::new(),
        }
    }

    /// Build the hierarchical config set for a run over `roots`, using
    /// `root_config` (already loaded by the caller) as `configs[0]`, then
    /// scanning the roots for every nested `poly.toml` and
    /// resolving each via the cascade.
    pub fn build(roots: &[PathBuf], root_config: Config) -> anyhow::Result<Self> {
        let primary = roots.first().cloned().unwrap_or_else(|| PathBuf::from("."));
        let root_dir = dir_of_root(&primary);

        let mut configs = vec![root_config];
        let mut dirs: Vec<Option<PathBuf>> = vec![Some(root_dir.clone())];
        let mut lookup: Vec<(PathBuf, usize)> = vec![(root_dir.clone(), 0)];
        let mut seen: HashSet<PathBuf> = HashSet::from([root_dir]);

        for dir in scan_config_dirs(roots) {
            if !seen.insert(dir.clone()) {
                continue;
            }
            let resolved: Config = poly_config::PolyConfig::resolve_for_dir(&dir)?.into();
            let id = configs.len();
            configs.push(resolved);
            dirs.push(Some(dir.clone()));
            lookup.push((dir, id));
        }
        lookup.sort_by_key(|(dir, _)| std::cmp::Reverse(dir.components().count()));
        Ok(Self { configs, dirs, lookup })
    }

    /// The id of the config governing `file`: the nearest ancestor config
    /// directory, or `0` (the root/fallback config) when none matches.
    pub fn config_id_for(&self, file: &Path) -> usize {
        let dir = file.parent().unwrap_or(file);
        for (config_dir, id) in &self.lookup {
            if dir.starts_with(config_dir) {
                return *id;
            }
        }
        0
    }

    /// Borrow the config with the given id.
    pub fn config(&self, id: usize) -> &Config {
        &self.configs[id]
    }

    /// Iterate the resolved configs in id order (for building per-config state).
    pub fn iter(&self) -> impl Iterator<Item = &Config> {
        self.configs.iter()
    }

    /// Number of resolved configs.
    pub fn len(&self) -> usize {
        self.configs.len()
    }

    /// Whether there are no configs (never true in practice; `configs[0]` always
    /// exists). Present to satisfy the `len`/`is_empty` clippy pairing.
    pub fn is_empty(&self) -> bool {
        self.configs.is_empty()
    }

    /// Exclude globs for the walk rooted at `root`: every directory-backed config
    /// under `root` contributes its own `[discovery] exclude` globs, each
    /// prefixed by the config directory relative to `root` (so a nested config
    /// only prunes its own subtree), unioned with `extra` (CLI `--exclude` / MCP
    /// globs, rooted at `root`).
    pub fn walk_excludes(&self, root: &Path, extra: &[String]) -> Vec<String> {
        let mut out = Vec::new();
        let mut contributed = false;
        for (config_dir, id) in &self.lookup {
            let Ok(rel) = config_dir.strip_prefix(root) else {
                continue;
            };
            contributed = true;
            for glob in &self.configs[*id].exclude {
                out.push(prefix_glob(rel, glob));
            }
        }
        if !contributed {
            out.extend(self.configs[0].exclude.iter().cloned());
        }
        out.extend(extra.iter().cloned());
        out
    }

    /// Bases for resolving a config's `[per-file-ignores]` globs: the config's
    /// own directory first (so a nested config's globs are relative to where it
    /// lives, matching ruff/eslint), then the run bases as a fallback.
    pub fn ignore_bases(&self, config_id: usize, run_bases: &[PathBuf]) -> Vec<PathBuf> {
        let mut bases = Vec::with_capacity(run_bases.len() + 1);
        if let Some(Some(dir)) = self.dirs.get(config_id) {
            bases.push(dir.clone());
        }
        bases.extend(run_bases.iter().cloned());
        bases
    }
}

/// The directory that anchors a run root: the path itself when it is (or looks
/// like) a directory, else its parent.
fn dir_of_root(path: &Path) -> PathBuf {
    if path.is_file() {
        path.parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| path.to_path_buf())
    } else {
        path.to_path_buf()
    }
}

/// Prefix a gitignore-style glob by `rel` (a config dir relative to the walk
/// root). An empty `rel` (the walk root itself) leaves the glob unchanged.
fn prefix_glob(rel: &Path, glob: &str) -> String {
    if rel.as_os_str().is_empty() {
        return glob.to_string();
    }
    let mut prefix = rel.to_string_lossy().replace('\\', "/");
    if !prefix.ends_with('/') {
        prefix.push('/');
    }
    format!("{prefix}{glob}")
}

/// Scan `roots` for every directory containing a config file, respecting
/// `.gitignore` and the same pruned-directory set as discovery.
fn scan_config_dirs(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut seen = HashSet::new();
    for root in roots {
        let mut builder = WalkBuilder::new(root);
        builder
            .hidden(false)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .parents(true)
            .filter_entry(keep_walk_entry);
        for entry in builder.build().flatten() {
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            let is_config = entry
                .file_name()
                .to_str()
                .is_some_and(|name| poly_config::CONFIG_FILE_NAMES.contains(&name));
            if is_config && let Some(dir) = entry.path().parent() {
                let dir = dir.to_path_buf();
                if seen.insert(dir.clone()) {
                    dirs.push(dir);
                }
            }
        }
    }
    dirs
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn single_config_maps_every_file_to_zero() {
        let set = ConfigSet::single(Config::default());
        assert_eq!(set.config_id_for(Path::new("/any/where/foo.rs")), 0);
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn nested_config_is_discovered_and_files_map_to_it() {
        let root = tempdir().unwrap();
        write(
            &root.path().join("poly.toml"),
            "[workspace]\nroot = true\n[defaults]\nline_length = 120\n",
        );
        write(
            &root.path().join("frontend/poly.toml"),
            "[defaults]\nline_length = 80\n",
        );
        write(&root.path().join("frontend/app.ts"), "const x = 1;\n");
        write(&root.path().join("src/main.rs"), "fn main() {}\n");

        let root_config: Config = poly_config::PolyConfig::resolve_for_dir(root.path()).unwrap().into();
        let set = ConfigSet::build(&[root.path().to_path_buf()], root_config).unwrap();

        let front_id = set.config_id_for(&root.path().join("frontend/app.ts"));
        let root_id = set.config_id_for(&root.path().join("src/main.rs"));
        assert_ne!(front_id, 0, "frontend file maps to the nested config");
        assert_eq!(root_id, 0, "root file maps to the root config");
        assert_eq!(set.config(front_id).defaults.line_length, 80, "nested override");
        assert_eq!(set.config(root_id).defaults.line_length, 120, "root default");
    }

    #[test]
    fn walk_excludes_root_nested_globs_at_their_config_dir() {
        let root = tempdir().unwrap();
        write(
            &root.path().join("poly.toml"),
            "[workspace]\nroot = true\n[discovery]\nexclude = [\"target/**\"]\n",
        );
        write(
            &root.path().join("frontend/poly.toml"),
            "[discovery]\nexclude = [\"dist/**\"]\n",
        );
        let root_config: Config = poly_config::PolyConfig::resolve_for_dir(root.path()).unwrap().into();
        let set = ConfigSet::build(&[root.path().to_path_buf()], root_config).unwrap();

        let excludes = set.walk_excludes(root.path(), &["extra/**".to_string()]);
        assert!(excludes.contains(&"target/**".to_string()), "root exclude unprefixed");
        assert!(
            excludes.contains(&"frontend/dist/**".to_string()),
            "nested exclude rooted at its config dir: {excludes:?}"
        );
        assert!(excludes.contains(&"extra/**".to_string()), "CLI extra passed through");
    }
}
