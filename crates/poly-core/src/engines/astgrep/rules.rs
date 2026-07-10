//! Rule-pack loading for the ast-grep custom-rule engine.
//!
//! [`load_rules`] scans the configured directories for `*.yml` / `*.yaml`
//! files, parses each with `ast_grep_config::from_yaml_string`, and groups the
//! results by language name into a [`RuleMap`].  The map is cached behind a
//! process-global `RwLock` keyed on the blake3 content hash of the rule files,
//! so a single `poly lint` run pays the file-read cost at most once per unique
//! rule set.
//!
//! ## Limitations
//!
//! - **No cross-file `refers:`.** Each YAML file is parsed with its own
//!   [`GlobalRules`], so a rule cannot reference a `utils`/global rule defined
//!   in a *different* file. Keep a rule and the utils it refers to in one file.
//!   (Project-level ast-grep `sgconfig.yml` utils are not wired in.)
//! - **Symlinked rule files are skipped.** Discovery uses `file_type()`
//!   (`lstat`), so a symlink — whether to a file or a directory — is neither
//!   read nor descended. Place real files under the rule dirs.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

use anyhow::Context;
use ast_grep_config::{GlobalRules, RuleConfig, from_yaml_string};

use super::language::TslpLanguage;

/// Rules grouped by lowercase language name.
pub type RuleMap = HashMap<String, Vec<RuleConfig<TslpLanguage>>>;

/// Process-global cache keyed on the blake3 content hash of the rule files.
/// The hash folds in each file's path + bytes, so it uniquely identifies the
/// resolved rule set: editing a rule file in a long-lived process (`poly mcp`)
/// yields a fresh entry, and distinct rule sets never collide. An `RwLock` lets
/// every rayon worker read concurrently on the hot (cache-hit) path; only the
/// first load of a hash takes the write lock.
static RULE_CACHE: OnceLock<RwLock<HashMap<String, Arc<RuleMap>>>> = OnceLock::new();

fn cache() -> &'static RwLock<HashMap<String, Arc<RuleMap>>> {
    RULE_CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Return the [`RuleMap`] for `dirs`, loading from disk on first call per unique
/// `content_hash` (the `rules_hash` folded into the engine config). Subsequent
/// calls with the same hash hit the cache — a `&str` lookup with no per-file key
/// allocation on the hot path.
///
/// An empty `content_hash` means no rule files were found (or none was supplied,
/// as in direct unit tests); that case loads fresh without caching, so two
/// distinct dir sets can never collide on an empty key.
///
/// Directories that do not exist are silently skipped (no error) — this is the
/// normal state for repos that haven't created any rule files yet.
pub fn load_rules(dirs: &[String], content_hash: &str) -> anyhow::Result<Arc<RuleMap>> {
    if content_hash.is_empty() {
        return Ok(Arc::new(load_from_dirs(dirs)?));
    }

    {
        let guard = cache().read().unwrap_or_else(|e| e.into_inner());
        if let Some(cached) = guard.get(content_hash) {
            return Ok(Arc::clone(cached));
        }
    }

    let arc = Arc::new(load_from_dirs(dirs)?);

    let mut guard = cache().write().unwrap_or_else(|e| e.into_inner());
    if let Some(existing) = guard.get(content_hash) {
        return Ok(Arc::clone(existing));
    }
    guard.insert(content_hash.to_string(), Arc::clone(&arc));
    Ok(arc)
}

/// Compute a stable hash of all rule files under `dirs` so rule edits
/// invalidate the cache key.  Returns an empty string when no rule files exist.
pub fn rules_hash(dirs: &[String]) -> String {
    let mut hasher = blake3::Hasher::new();
    let mut had_any = false;

    let mut paths: Vec<PathBuf> = dirs.iter().flat_map(|d| collect_rule_paths(Path::new(d))).collect();
    paths.sort();

    for path in paths {
        if let Ok(bytes) = fs::read(&path) {
            hasher.update(path.to_string_lossy().as_bytes());
            hasher.update(&bytes);
            had_any = true;
        }
    }

    if had_any {
        hasher.finalize().to_hex().to_string()
    } else {
        String::new()
    }
}

/// Load every rule from `dirs` into a flat list (rule id order is the on-disk
/// sort order). Test files (`*-test.yml`) are skipped. Used by the rule-test
/// runner, which needs id → rule lookup rather than the language grouping.
pub fn load_flat(dirs: &[String]) -> anyhow::Result<Vec<RuleConfig<TslpLanguage>>> {
    let globals = GlobalRules::default();
    let mut out = Vec::new();
    for dir in dirs {
        for path in collect_rule_paths(Path::new(dir)) {
            let yaml = fs::read_to_string(&path).with_context(|| format!("reading rule file {}", path.display()))?;
            let rules: Vec<RuleConfig<TslpLanguage>> = from_yaml_string(&yaml, &globals)
                .with_context(|| format!("parsing ast-grep rules in {}", path.display()))?;
            out.extend(rules);
        }
    }
    Ok(out)
}

fn load_from_dirs(dirs: &[String]) -> anyhow::Result<RuleMap> {
    let mut map: RuleMap = HashMap::new();
    for rule in load_flat(dirs)? {
        map.entry(rule.language.name().to_string()).or_default().push(rule);
    }
    Ok(map)
}

/// A YAML file is a *test* file when its name ends in `-test.yml` / `-test.yaml`
/// (ast-grep's convention). Test files hold `valid`/`invalid` snippets, not rules.
fn is_test_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.ends_with("-test.yml") || n.ends_with("-test.yaml"))
        .unwrap_or(false)
}

fn is_yaml(path: &Path) -> bool {
    path.extension()
        .map(|ext| ext == "yml" || ext == "yaml")
        .unwrap_or(false)
}

/// Recursively collect every `*.yml` / `*.yaml` path under `dir`, sorted.
fn collect_yaml_paths(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_yaml_rec(dir, &mut out);
    out.sort();
    out
}

fn collect_yaml_rec(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_dir() {
            collect_yaml_rec(&path, out);
        } else if file_type.is_file() && is_yaml(&path) {
            out.push(path);
        }
    }
}

/// Rule files under `dir` (recursive) — every `*.yml`/`*.yaml` except test files.
fn collect_rule_paths(dir: &Path) -> Vec<PathBuf> {
    collect_yaml_paths(dir)
        .into_iter()
        .filter(|p| !is_test_file(p))
        .collect()
}

/// Test files (`*-test.yml`) under any of `dirs` (recursive).
pub fn collect_test_paths(dirs: &[String]) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = dirs
        .iter()
        .flat_map(|d| collect_yaml_paths(Path::new(d)))
        .filter(|p| is_test_file(p))
        .collect();
    paths.sort();
    paths.dedup();
    paths
}
