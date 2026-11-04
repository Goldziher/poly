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
//! - **Cross-file `matches:` goes through `utils.yml`.** A file named
//!   `utils.yml` / `utils.yaml` in a rule directory is not a rule file: it
//!   declares *global* utility rules (a YAML list, each with an `id` and a
//!   `language`) that any rule in the same load may reference by `matches:`.
//!   Every such file in the searched dirs is merged into one [`GlobalRules`],
//!   so utils may reference each other across files — ast-grep topologically
//!   sorts them. A per-file `utils:` block still works and stays file-local.
//!   (Project-level ast-grep `sgconfig.yml` utils are not wired in.)
//! - **Symlinked rule files are skipped.** Discovery uses `file_type()`
//!   (`lstat`), so a symlink — whether to a file or a directory — is neither
//!   read nor descended. Place real files under the rule dirs.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

use anyhow::Context;
use ast_grep_config::{DeserializeEnv, GlobalRules, RuleConfig, SerializableGlobalRule, from_yaml_string};

pub use poly_engine_api::host_hooks::astgrep_rules::rules_hash;
use poly_engine_api::host_hooks::astgrep_rules::{
    collect_rule_paths, collect_utils_paths, collect_yaml_paths, is_test_file,
};

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

/// Load every rule from `dirs` into a flat list, paired with the file it came
/// from (rule id order is the on-disk sort order). Test files (`*-test.yml`)
/// are skipped.
///
/// The path lets a caller disambiguate rules that deliberately share an `id`
/// across different rule files — poly's built-in pack ships `todo-marker`
/// once per language, each in its own directory — which a bare `id` lookup
/// cannot. See [`super::test::run_tests`], the one caller that needs this.
pub fn load_flat_with_paths(dirs: &[String]) -> anyhow::Result<Vec<(PathBuf, RuleConfig<TslpLanguage>)>> {
    let utils_paths: Vec<PathBuf> = dirs
        .iter()
        .flat_map(|dir| collect_utils_paths(Path::new(dir)))
        .collect();
    let mut sources = Vec::with_capacity(utils_paths.len());
    for path in &utils_paths {
        let yaml = fs::read_to_string(path).with_context(|| format!("reading utils file {}", path.display()))?;
        sources.push((path.clone(), yaml));
    }
    let globals = parse_global_utils(&sources)?;

    let mut out = Vec::new();
    for dir in dirs {
        for path in collect_rule_paths(Path::new(dir)) {
            let yaml = fs::read_to_string(&path).with_context(|| format!("reading rule file {}", path.display()))?;
            let rules: Vec<RuleConfig<TslpLanguage>> = from_yaml_string(&yaml, &globals)
                .with_context(|| format!("parsing ast-grep rules in {}", path.display()))?;
            out.extend(rules.into_iter().map(|rule| (path.clone(), rule)));
        }
    }
    Ok(out)
}

/// Merge every `utils.yml` into one [`GlobalRules`] the whole load shares.
///
/// One registration rather than one per file is what makes a util referenceable
/// across files; ast-grep topologically sorts the merged set, so a util may
/// refer to one declared in a different file regardless of read order.
///
/// # Errors
/// A malformed utils file is an error rather than a skip: silently dropping it
/// would leave every `matches:` that referenced it unresolvable, and the rule
/// carrying that reference would fail with an error naming the *rule* instead
/// of the file that actually broke.
pub(super) fn parse_global_utils(sources: &[(PathBuf, String)]) -> anyhow::Result<GlobalRules> {
    let mut all = Vec::new();
    for (path, yaml) in sources {
        let utils: Vec<SerializableGlobalRule<TslpLanguage>> = ast_grep_config::from_str(yaml)
            .with_context(|| format!("parsing ast-grep global utils in {}", path.display()))?;
        all.extend(utils);
    }
    if all.is_empty() {
        return Ok(GlobalRules::default());
    }
    DeserializeEnv::parse_global_utils(all)
        .map_err(|error| anyhow::anyhow!("registering ast-grep global utils: {error}"))
}

/// Load every rule from `dirs` into a flat list (rule id order is the on-disk
/// sort order), discarding the source path. Used by callers (rule listing,
/// language grouping) that only need id → rule, not the disambiguation
/// [`load_flat_with_paths`] provides.
pub fn load_flat(dirs: &[String]) -> anyhow::Result<Vec<RuleConfig<TslpLanguage>>> {
    Ok(load_flat_with_paths(dirs)?.into_iter().map(|(_, rule)| rule).collect())
}

fn load_from_dirs(dirs: &[String]) -> anyhow::Result<RuleMap> {
    let mut map: RuleMap = HashMap::new();
    for rule in load_flat(dirs)? {
        map.entry(rule.language.name().to_string()).or_default().push(rule);
    }
    Ok(map)
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
