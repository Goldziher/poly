//! Rule-pack loading for the ast-grep custom-rule engine.
//!
//! [`load_rules`] scans the configured directories for `*.yml` / `*.yaml`
//! files, parses each with `ast_grep_config::from_yaml_string`, and groups the
//! results by language name into a [`RuleMap`].  The map is cached behind a
//! process-global `Mutex` keyed on the resolved directory list, so a single
//! `poly lint` run pays the file-read cost at most once per unique `[rules]
//! dirs` configuration.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

use anyhow::Context;
use ast_grep_config::{GlobalRules, RuleConfig, from_yaml_string};

use super::language::TslpLanguage;

/// Rules grouped by lowercase language name.
pub type RuleMap = HashMap<String, Vec<RuleConfig<TslpLanguage>>>;

/// Cache key: the configured dirs plus a content hash of the rule files. The
/// hash makes the cache content-addressed, so editing a rule file in a
/// long-lived process (`poly mcp`) yields a fresh entry instead of a stale one;
/// the dirs keep distinct rule sets (e.g. two test temp dirs) from colliding
/// when no hash is supplied.
type CacheKey = (Vec<String>, String);

/// Process-global cache. An `RwLock` lets every rayon worker read concurrently
/// on the hot (cache-hit) path; only the first load of a key takes the write lock.
static RULE_CACHE: OnceLock<RwLock<HashMap<CacheKey, Arc<RuleMap>>>> = OnceLock::new();

fn cache() -> &'static RwLock<HashMap<CacheKey, Arc<RuleMap>>> {
    RULE_CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Return the [`RuleMap`] for `dirs`, loading from disk on first call per
/// unique `(dirs, content_hash)` key. `content_hash` is the `rules_hash` folded
/// into the engine config; pass `""` when no hash is available (the dirs alone
/// then key the entry). Subsequent calls with the same key hit the cache.
///
/// Directories that do not exist are silently skipped (no error) — this is the
/// normal state for repos that haven't created any rule files yet.
pub fn load_rules(dirs: &[String], content_hash: &str) -> anyhow::Result<Arc<RuleMap>> {
    let key: CacheKey = (dirs.to_vec(), content_hash.to_string());

    {
        // Recover from a poisoned lock rather than panicking across the worker
        // pool: a poisoned rule cache still holds valid (immutable) entries.
        let guard = cache().read().unwrap_or_else(|e| e.into_inner());
        if let Some(cached) = guard.get(&key) {
            return Ok(Arc::clone(cached));
        }
    }

    // Cache miss: load from disk, then insert under the write lock.
    let arc = Arc::new(load_from_dirs(dirs)?);

    let mut guard = cache().write().unwrap_or_else(|e| e.into_inner());
    // Check again in case another thread raced us to load the same key.
    if let Some(existing) = guard.get(&key) {
        return Ok(Arc::clone(existing));
    }
    guard.insert(key, Arc::clone(&arc));
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

// ── internals ─────────────────────────────────────────────────────────────────

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
        // `file_type()` does NOT follow symlinks, so a symlinked directory is
        // never descended into — this bounds recursion and avoids symlink-cycle
        // stack overflows (real dirs only).
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
