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
    /// Absolute, canonicalized directory where the run's root config
    /// (`configs[0]`) file lives, when known. This may be an *ancestor* of the
    /// walk root — running `poly` from a repo subdirectory (`frontend/`) while
    /// the governing `poly.toml` sits at the repo root — in which case the root
    /// config's exclude / per-file-ignore globs are anchored there and must be
    /// re-anchored to the walk root. `None` for the single-config (`--config`)
    /// bypass and when no config file backs the run.
    root_config_dir: Option<PathBuf>,
}

impl ConfigSet {
    /// A single config applied to every file — the `--config <path>` bypass (and
    /// the conformance harness). No nested resolution.
    pub fn single(config: Config) -> Self {
        Self {
            configs: vec![config],
            dirs: vec![None],
            lookup: Vec::new(),
            root_config_dir: None,
        }
    }

    /// Build the hierarchical config set for a run over `roots`, using
    /// `root_config` (already loaded by the caller) as `configs[0]`, then
    /// scanning the roots for every nested `poly.toml` and
    /// resolving each via the cascade.
    pub fn build(roots: &[PathBuf], root_config: Config) -> anyhow::Result<Self> {
        let primary = roots.first().cloned().unwrap_or_else(|| PathBuf::from("."));
        let root_dir = dir_of_root(&primary);
        let root_config_dir = root_config_dir(&root_dir);

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
        Ok(Self {
            configs,
            dirs,
            lookup,
            root_config_dir,
        })
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

    /// Exclude globs for the walk rooted at `root`: the run's root config
    /// contributes its `[discovery] exclude` globs re-anchored to `root` (see
    /// [`root_config_excludes`]), and every *nested* directory-backed config
    /// under `root` contributes its own globs prefixed by the config directory
    /// relative to `root` (so a nested config only prunes its own subtree),
    /// unioned with `extra` (CLI `--exclude` / MCP globs, rooted at `root`).
    ///
    /// [`root_config_excludes`]: ConfigSet::root_config_excludes
    pub fn walk_excludes(&self, root: &Path, extra: &[String]) -> Vec<String> {
        let mut out = self.root_config_excludes(root);
        for (config_dir, id) in &self.lookup {
            if *id == 0 {
                continue;
            }
            if let Ok(rel) = config_dir.strip_prefix(root) {
                for glob in &self.configs[*id].exclude {
                    out.push(prefix_glob(rel, glob));
                }
            }
        }
        out.extend(extra.iter().cloned());
        out
    }

    /// The run root config's exclude globs, re-anchored from the directory where
    /// its config file lives to the walk `root`.
    ///
    /// When `poly` runs from a repo subdirectory, that config directory is an
    /// *ancestor* of the walk root, so each glob is stripped of the subpath from
    /// the config directory down to the walk root (globs targeting sibling
    /// subtrees, which can never match under the walk root, are dropped — see
    /// [`reanchor_glob`]). When the config directory *is* the walk root (the
    /// common whole-repo run) the globs are emitted unchanged.
    fn root_config_excludes(&self, root: &Path) -> Vec<String> {
        let globs = &self.configs[0].exclude;
        let Some(config_dir) = &self.root_config_dir else {
            return globs.clone();
        };
        let Ok(abs_root) = std::fs::canonicalize(root) else {
            return globs.clone();
        };
        match abs_root.strip_prefix(config_dir) {
            Ok(sub) if sub.as_os_str().is_empty() => globs.clone(),
            Ok(sub) => globs.iter().filter_map(|glob| reanchor_glob(sub, glob)).collect(),
            Err(_) => globs.clone(),
        }
    }

    /// Bases for resolving a config's `[per-file-ignores]` globs: the config's
    /// own directory first (so a nested config's globs are relative to where it
    /// lives, matching ruff/eslint), then the run bases as a fallback.
    ///
    /// For the run root config (`config_id == 0`) the resolved
    /// [`root_config_dir`] is preferred over the walk root, so a subdirectory run
    /// still matches per-file-ignore globs anchored at the repo-root config.
    ///
    /// [`root_config_dir`]: ConfigSet::root_config_dir
    pub fn ignore_bases(&self, config_id: usize, run_bases: &[PathBuf]) -> Vec<PathBuf> {
        let mut bases = Vec::with_capacity(run_bases.len() + 1);
        let anchor = if config_id == 0 && self.root_config_dir.is_some() {
            self.root_config_dir.clone()
        } else {
            self.dirs.get(config_id).cloned().flatten()
        };
        if let Some(dir) = anchor {
            bases.push(dir);
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

/// The absolute, canonicalized directory whose `poly.toml` governs `walk_root`:
/// the nearest ancestor (at or above `walk_root`) that contains a config file,
/// bounded at the git repository root so the search never climbs past the
/// repository. Returns `None` when `walk_root` cannot be canonicalized or no
/// config file is found within the boundary (in which case the run root config
/// carries no re-anchorable excludes).
fn root_config_dir(walk_root: &Path) -> Option<PathBuf> {
    let mut current = Some(std::fs::canonicalize(walk_root).ok()?);
    while let Some(dir) = current {
        if poly_config::CONFIG_FILE_NAMES
            .iter()
            .any(|name| dir.join(name).is_file())
        {
            return Some(dir);
        }
        if dir.join(".git").exists() {
            break;
        }
        current = dir.parent().map(Path::to_path_buf);
    }
    None
}

/// Re-anchor an ancestor config's exclude `glob` to a walk root nested `sub`
/// below the config directory. `sub` is the walk root relative to the config
/// directory (e.g. `frontend`); `glob` is a gitignore-style, `/`-separated
/// pattern (Windows `\` normalized like [`prefix_glob`]).
///
/// - A pattern anchored inside the walk-root subtree (`frontend/src/data/**`
///   with `sub` = `frontend`) is stripped of the `sub/` prefix →
///   `src/data/**`, so it matches files scanned relative to the walk root.
/// - An un-anchored pattern (leading `**/`, or a bare name with no separator
///   such as `.secrets.baseline`) matches at any depth and is kept unchanged.
/// - A single leading directory segment with a recursive wildcard
///   (`target/**`, `node_modules/**`) is a build/vendor prune that still applies
///   under the walk root, so it is kept.
/// - A deeper concrete sibling path (`services/api/**`) can never match anything
///   under the walk root, so it is dropped (`None`).
fn reanchor_glob(sub: &Path, glob: &str) -> Option<String> {
    let glob = glob.replace('\\', "/");
    // Un-anchored patterns apply at any depth, so they hold under the walk root.
    if glob.starts_with("**") || !glob.contains('/') {
        return Some(glob);
    }
    let sub = sub.to_string_lossy().replace('\\', "/");
    let sub = sub.trim_end_matches('/');
    if glob == sub {
        // The glob excludes the entire walk-root subtree.
        return Some("**".to_string());
    }
    if let Some(rest) = glob.strip_prefix(&format!("{sub}/")) {
        // Anchored inside the walk-root subtree → drop the `sub/` prefix.
        return Some(if rest.is_empty() {
            "**".to_string()
        } else {
            rest.to_string()
        });
    }
    // Anchored elsewhere: keep a single-directory recursive prune (applies under
    // the walk root too); drop a deeper concrete sibling path (cannot match).
    match glob.split_once('/') {
        Some((_, rest)) if rest.is_empty() || rest == "**" => Some(glob),
        _ => None,
    }
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

    #[test]
    fn walk_excludes_reanchors_ancestor_root_config_from_subdir() {
        // Root config lives at the repo root; the walk root is a subdirectory
        // (`frontend/`). The root config's exclude globs are anchored at the repo
        // root and must be re-anchored to the walk root.
        let root = tempdir().unwrap();
        write(
            &root.path().join("poly.toml"),
            "[workspace]\nroot = true\n[discovery]\nexclude = [\
             \"frontend/src/data/benchmark/**\", \
             \"frontend/src/types/api-schema.d.ts\", \
             \"**/*.min.js\", \
             \"target/**\", \
             \"services/api/**\"]\n",
        );
        let frontend = root.path().join("frontend");
        fs::create_dir_all(frontend.join("src")).unwrap();

        let root_config: Config = poly_config::PolyConfig::resolve_for_dir(&frontend).unwrap().into();
        let set = ConfigSet::build(std::slice::from_ref(&frontend), root_config).unwrap();

        let excludes = set.walk_excludes(&frontend, &[]);
        assert!(
            excludes.contains(&"src/data/benchmark/**".to_string()),
            "sub-anchored glob re-anchored to the walk root: {excludes:?}"
        );
        assert!(
            excludes.contains(&"src/types/api-schema.d.ts".to_string()),
            "sub-anchored file re-anchored to the walk root: {excludes:?}"
        );
        assert!(
            excludes.contains(&"**/*.min.js".to_string()),
            "any-depth glob preserved: {excludes:?}"
        );
        assert!(
            excludes.contains(&"target/**".to_string()),
            "single-segment recursive prune preserved: {excludes:?}"
        );
        assert!(
            !excludes.iter().any(|glob| glob.contains("services")),
            "sibling-subtree glob dropped: {excludes:?}"
        );
        // The un-re-anchored form must not leak through.
        assert!(
            !excludes.iter().any(|glob| glob.starts_with("frontend/")),
            "no repo-root-anchored globs remain: {excludes:?}"
        );
    }

    #[test]
    fn reanchor_glob_classifies_patterns() {
        let sub = Path::new("frontend");
        assert_eq!(
            reanchor_glob(sub, "frontend/src/data/**").as_deref(),
            Some("src/data/**")
        );
        assert_eq!(reanchor_glob(sub, "frontend/x.ts").as_deref(), Some("x.ts"));
        assert_eq!(reanchor_glob(sub, "**/*.min.js").as_deref(), Some("**/*.min.js"));
        assert_eq!(
            reanchor_glob(sub, ".secrets.baseline").as_deref(),
            Some(".secrets.baseline")
        );
        assert_eq!(
            reanchor_glob(sub, "node_modules/**").as_deref(),
            Some("node_modules/**")
        );
        assert_eq!(reanchor_glob(sub, "services/api/**"), None);
        assert_eq!(reanchor_glob(sub, "crates/x-ffi/include/*.h"), None);
    }
}
