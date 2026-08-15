//! Hierarchical, monorepo-aware config resolution (ADR 0018).
//!
//! A run may span several projects, each with its own `poly.toml`. This module
//! discovers every in-tree config, resolves each via the cascade in
//! [`poly_config::PolyConfig::resolve_for_dir`] (nearest config wins, inheriting
//! from ancestors up to the workspace root), and maps each discovered file to
//! the nearest config that governs it.
//!
//! Which config governs a file is a property of **the file**, not of how the run
//! was invoked: `poly fmt .`, `poly fmt packages/app`, and `poly fmt
//! packages/app/src/x.py` must all apply `packages/app/poly.toml` to that file.
//! So the config set registers configs found *below* the walked paths **and**
//! the chain of configs *above* them ([`config_dirs_governing`]) — the latter
//! being what a run rooted inside a sub-project, or a hook handed explicit
//! staged paths, depends on entirely.
//!
//! The run's root config (loaded by the caller) is `configs[0]` and remains the
//! fallback for any file no registered config directory covers. A repo with a
//! single root `poly.toml` and no nested configs resolves every file to that one
//! config — byte-for-byte the pre-hierarchical behavior — though it reaches it
//! through the registered entry for the root directory rather than through
//! `configs[0]`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

use crate::config::Config;
use crate::discover::keep_walk_entry;

/// The resolved configs in effect for a run, plus the directory→config map used
/// to associate each file with the nearest config that governs it.
pub struct ConfigSet {
    /// Deduped resolved configs. `configs[0]` is the run's root config (loaded by
    /// the caller) and the fallback for any file no entry of `lookup` covers.
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
    /// Whether some entry of `lookup` sits at [`root_config_dir`], in which case
    /// that entry already carries `configs[0]`'s `[discovery] exclude` globs.
    /// Precomputed because `walk_excludes` runs once per root, and a hook run has
    /// one root per staged file.
    root_config_registered: bool,
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
            root_config_registered: false,
        }
    }

    /// Build the hierarchical config set for a run over `roots`, using
    /// `root_config` (already loaded by the caller) as `configs[0]`, then
    /// registering every `poly.toml` that governs those roots — both the nested
    /// ones found by scanning *below* them and the chain of ancestors *above*
    /// them — resolving each via the cascade.
    pub fn build(roots: &[PathBuf], root_config: Config) -> anyhow::Result<Self> {
        Self::build_with(roots, root_config, &poly_config::LocalPathResolver)
    }

    /// [`build`](ConfigSet::build) with an explicit resolver for nested configs'
    /// `extends` bases. Each nested `poly.toml` resolves its own `extends`
    /// (remote or local) through `resolver` before the cascade merges it.
    pub fn build_with(
        roots: &[PathBuf],
        root_config: Config,
        resolver: &dyn poly_config::BaseConfigResolver,
    ) -> anyhow::Result<Self> {
        let primary = roots.first().cloned().unwrap_or_else(|| PathBuf::from("."));
        let root_dir = dir_of_root(&primary);
        let root_config_dir = shared_root_config_dir(roots).or_else(|| root_config_dir(&root_dir));

        let mut configs = vec![root_config];
        let mut dirs: Vec<Option<PathBuf>> = vec![Some(root_dir)];
        let mut lookup: Vec<(PathBuf, usize)> = Vec::new();
        let mut seen: HashSet<PathBuf> = HashSet::new();

        for dir in config_dirs_governing(roots, resolver)? {
            if !seen.insert(canonical_key(&dir)) {
                continue;
            }
            let resolved: Config = poly_config::PolyConfig::resolve_for_dir_with(&dir, resolver)?.into();
            let id = configs.len();
            configs.push(resolved);
            dirs.push(Some(dir.clone()));
            lookup.push((dir, id));
        }
        lookup.sort_by_key(|(dir, _)| std::cmp::Reverse(dir.components().count()));
        let root_config_registered = root_config_dir.as_ref().is_some_and(|root_config_dir| {
            let key = canonical_key(root_config_dir);
            lookup.iter().any(|(dir, _)| canonical_key(dir) == key)
        });
        Ok(Self {
            configs,
            dirs,
            lookup,
            root_config_dir,
            root_config_registered,
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

    /// The directory an exclude glob for `root` is both *written in* and
    /// *matched in* — the two must agree or a glob silently matches nothing.
    ///
    /// A walked directory is its own frame: the globs are relative to it and the
    /// walk matcher is rooted there.
    ///
    /// An explicitly named **file** has no walk, so it is matched in one shot
    /// against a frame that has to be wide enough to see every path component a
    /// glob might name. That frame is the **outermost** config directory
    /// governing the file — the workspace root — not the file's own directory: a
    /// glob like `generated/**` names a directory *between* the two, which a
    /// frame anchored at the file's parent has already consumed and can never
    /// match. Every governing config then sits at or below the frame, so each
    /// contributes by simple prefixing.
    pub fn match_frame(&self, root: &Path) -> PathBuf {
        let dir = dir_of_root(root);
        if !root.is_file() {
            return dir;
        }
        self.lookup
            .iter()
            .filter(|(config_dir, _)| relative_descent(&dir, config_dir).is_some())
            .min_by_key(|(config_dir, _)| config_dir.components().count())
            .map(|(config_dir, _)| config_dir.clone())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or(dir)
    }

    /// Exclude globs for the run rooted at `root`, expressed relative to its
    /// [`match_frame`](ConfigSet::match_frame).
    ///
    /// Every directory-backed config that governs the frame contributes its own
    /// `[discovery] exclude`, re-expressed in the frame:
    ///
    /// - a config *under* the frame is prefixed by its directory relative to the
    ///   frame, so it prunes only its own subtree ([`prefix_glob`]);
    /// - a config *above* the frame — the ancestor chain a walked sub-project
    ///   directory sits inside — is re-anchored down to the frame
    ///   ([`reanchor_glob`]), dropping globs that can never match under it.
    ///
    /// The run's root config ([`root_config_excludes`]) contributes only when no
    /// registered config already sits at its directory, which is what keeps the
    /// single-config repo from compiling every glob twice.
    ///
    /// `extra` (CLI `--exclude` / MCP globs, already in the frame) is unioned in
    /// last. When `exclude_root` is false, only a rule covering the named root
    /// itself is removed; exclusions for descendants remain active.
    ///
    /// [`root_config_excludes`]: ConfigSet::root_config_excludes
    pub fn walk_excludes(&self, root: &Path, extra: &[String], exclude_root: bool) -> Vec<String> {
        let frame = self.match_frame(root);
        let mut out = if self.root_config_registered {
            Vec::new()
        } else {
            self.root_config_excludes(&frame)
        };
        for (config_dir, id) in &self.lookup {
            let globs = &self.configs[*id].exclude;
            if globs.is_empty() {
                continue;
            }
            if let Some(rel) = relative_descent(config_dir, &frame) {
                out.extend(globs.iter().map(|glob| prefix_glob(&rel, glob)));
            } else if let Some(sub) = relative_descent(&frame, config_dir) {
                out.extend(globs.iter().filter_map(|glob| reanchor_glob(&sub, glob)));
            }
        }
        out.extend(extra.iter().cloned());
        if !exclude_root {
            out.retain(|glob| glob != "**" && glob != "/**");
        }
        out
    }

    /// The run root config's exclude globs, re-anchored from the directory where
    /// its config file lives to the walk frame.
    ///
    /// When `poly` runs from a repo subdirectory, that config directory is an
    /// *ancestor* of the walk frame, so each glob is stripped of the subpath from
    /// the config directory down to the frame (globs targeting sibling subtrees,
    /// which can never match under the frame, are dropped — see
    /// [`reanchor_glob`]). When the config directory *is* the frame (the common
    /// whole-repo run) the globs are emitted unchanged.
    fn root_config_excludes(&self, frame: &Path) -> Vec<String> {
        let globs = &self.configs[0].exclude;
        let Some(config_dir) = &self.root_config_dir else {
            return globs.clone();
        };
        let Ok(abs_root) = std::fs::canonicalize(frame) else {
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
///
/// This is the frame every exclude glob for that root is expressed in, so
/// discovery resolves an explicitly named file against the same directory the
/// config set anchored it to.
pub(crate) fn dir_of_root(path: &Path) -> PathBuf {
    if path.is_file() {
        nonempty_parent(path)
    } else {
        path.to_path_buf()
    }
}

fn nonempty_parent(path: &Path) -> PathBuf {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn shared_root_config_dir(roots: &[PathBuf]) -> Option<PathBuf> {
    let root_dirs: Vec<PathBuf> = roots
        .iter()
        .filter_map(|root| std::fs::canonicalize(dir_of_root(root)).ok())
        .collect();
    root_dirs
        .iter()
        .filter_map(|root| root_config_dir(root))
        .filter(|config_dir| root_dirs.iter().all(|root| root.starts_with(config_dir)))
        .max_by_key(|config_dir| config_dir.components().count())
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
    if glob.starts_with("**") {
        return unanchored_pattern_covers_root(sub, &glob)
            .then(|| "**".to_string())
            .or(Some(glob));
    }
    if !glob.contains('/') {
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

fn unanchored_pattern_covers_root(sub: &Path, glob: &str) -> bool {
    let Some(target) = glob.strip_prefix("**/") else {
        return glob == "**";
    };
    let target = target.strip_suffix("/**").unwrap_or(target);
    !target.contains(['*', '?', '[']) && sub.ends_with(Path::new(target))
}

/// Every directory whose `poly.toml` governs any part of `roots`: the configs
/// *below* the roots (found by [`scan_config_dirs`]) plus, for each root, the
/// chain of configs *above* it.
///
/// The ancestor half is what makes hierarchical resolution a property of the
/// file rather than of the invocation. Scanning alone only ever finds configs at
/// or below a walk root, so `poly fmt packages/app/src/x.py` — and every hook
/// run, which is handed explicit staged paths — saw no nested config at all and
/// fell back to the run's root config: the sub-project's `[discovery] exclude`,
/// `[defaults]` and `[per-file-ignores]` were silently inert.
///
/// Ancestor chains are resolved once per distinct directory frame, not once per
/// root, because a hook run passes one root per staged file and those collapse
/// onto a handful of directories.
fn config_dirs_governing(
    roots: &[PathBuf],
    resolver: &dyn poly_config::BaseConfigResolver,
) -> anyhow::Result<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    let mut frames = HashSet::new();
    for root in roots {
        let frame = dir_of_root(root);
        if !frames.insert(frame.clone()) {
            continue;
        }
        dirs.extend(
            poly_config::config_chain_dirs_with(&frame, resolver)?
                .into_iter()
                .map(normalize_dir),
        );
    }
    dirs.extend(scan_config_dirs(roots));
    Ok(dirs)
}

/// Spell the current directory `.` rather than the empty path, which a relative
/// upward walk bottoms out at. The two are the same directory, but only `.`
/// composes with the path arithmetic in [`relative_descent`].
fn normalize_dir(dir: PathBuf) -> PathBuf {
    if dir.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        dir
    }
}

/// A stable identity for a directory, so the same directory reached by different
/// spellings (relative vs absolute, via a symlink) is registered once. Falls back
/// to the path as written when it cannot be canonicalized.
fn canonical_key(dir: &Path) -> PathBuf {
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf())
}

/// `descendant` expressed relative to `ancestor`, or `None` when it does not sit
/// under it. Tries the paths as written first — the common case, where both come
/// from the same run root and share a spelling — and only pays for
/// canonicalization when that fails.
fn relative_descent(descendant: &Path, ancestor: &Path) -> Option<PathBuf> {
    if let Ok(relative) = descendant.strip_prefix(ancestor) {
        return Some(relative.to_path_buf());
    }
    let descendant = std::fs::canonicalize(descendant).ok()?;
    let ancestor = std::fs::canonicalize(ancestor).ok()?;
    descendant.strip_prefix(ancestor).ok().map(Path::to_path_buf)
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
    fn relative_file_root_uses_the_current_directory_as_its_anchor() {
        assert_eq!(nonempty_parent(Path::new("included.py")), PathBuf::from("."));
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
        // Which *slot* a file lands in is an internal detail — the repo-root
        // config is now registered in its own right (so that a run rooted below
        // it still finds it), rather than being reachable only as `configs[0]`.
        // What must hold is that the two subtrees resolve differently, and to
        // the values their configs declare.
        assert_ne!(front_id, root_id, "the two subtrees must resolve to different configs");
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

        let excludes = set.walk_excludes(root.path(), &["extra/**".to_string()], true);
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

        let excludes = set.walk_excludes(&frontend, &[], true);
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

        let snippets = Path::new("docs-site/src/snippets");
        assert_eq!(reanchor_glob(snippets, "**/snippets/**").as_deref(), Some("**"));
        assert_eq!(
            reanchor_glob(snippets, "**/docs-site/src/snippets/**").as_deref(),
            Some("**")
        );
        assert_eq!(
            reanchor_glob(snippets, "**/generated/**").as_deref(),
            Some("**/generated/**")
        );
        assert_eq!(reanchor_glob(snippets, "**/*.tmp").as_deref(), Some("**/*.tmp"));
    }
}
