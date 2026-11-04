//! ast-grep rule-file discovery and the content hash of a rule set.
//!
//! The hash is folded into the ast-grep engine's config, and so into its cache key, by the host's
//! config resolution. It lives here rather than in the engine family so a core build, which serves
//! ast-grep from a component, keys the cache on the same rule files without linking the engine.

use std::fs;
use std::path::{Path, PathBuf};

/// Compute a stable hash of all rule files under `dirs` so rule edits
/// invalidate the cache key.  Returns an empty string when no rule files exist.
pub fn rules_hash(dirs: &[String]) -> String {
    let mut hasher = blake3::Hasher::new();
    let mut had_any = false;

    // Utils files are inputs to the parse, so an edit to one must invalidate
    // the cache exactly as a rule edit does — hash the rule files *and* the
    // `utils.yml` they resolve against, not just the former.
    let mut paths: Vec<PathBuf> = dirs
        .iter()
        .flat_map(|d| {
            let dir = Path::new(d);
            collect_rule_paths(dir).into_iter().chain(collect_utils_paths(dir))
        })
        .collect();
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

/// A YAML file is a *test* file when its name ends in `-test.yml` / `-test.yaml`
/// (ast-grep's convention). Test files hold `valid`/`invalid` snippets, not rules.
pub fn is_test_file(path: &Path) -> bool {
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
pub fn collect_yaml_paths(dir: &Path) -> Vec<PathBuf> {
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

/// A YAML file declares *global utils* when it is named `utils.yml` /
/// `utils.yaml`. Such a file holds a list of utility rules, not rule configs,
/// so it must never reach `from_yaml_string`.
fn is_utils_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n == "utils.yml" || n == "utils.yaml")
        .unwrap_or(false)
}

/// Rule files under `dir` (recursive) — every `*.yml`/`*.yaml` except test
/// files and `utils.yml`.
pub fn collect_rule_paths(dir: &Path) -> Vec<PathBuf> {
    collect_yaml_paths(dir)
        .into_iter()
        .filter(|p| !is_test_file(p) && !is_utils_file(p))
        .collect()
}

/// Global-utils files under `dir` (recursive), sorted.
pub fn collect_utils_paths(dir: &Path) -> Vec<PathBuf> {
    collect_yaml_paths(dir)
        .into_iter()
        .filter(|p| is_utils_file(p))
        .collect()
}

/// `(rule id, globs)` pairs a rule set declares through ast-grep's `ignores:` key; the runner merges
/// them into its per-file ignores as defaults.
pub type DefaultPathIgnores = Vec<(String, Vec<globset::Glob>)>;

/// The built-in pack's declared `ignores:`, as `[{ "id", "ignores" }]` for each rule that has any.
///
/// The pack's YAML lives in the ast-grep family, which a core build does not link, so its
/// exclusions are committed here; a drift test in `poly-engines-astgrep` regenerates this file
/// from the pack and fails when it is stale.
pub const PACK_IGNORES_JSON: &str = include_str!("../../data/astgrep_pack_ignores.json");

/// One `ignores:` entry: a bare glob or ast-grep's `{ glob, caseInsensitive }` form.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(untagged)]
pub enum IgnoreGlob {
    /// A bare glob.
    Plain(String),
    /// A glob with its case sensitivity.
    Config {
        /// The glob.
        glob: String,
        /// Whether the glob matches case-insensitively.
        #[serde(rename = "caseInsensitive", default)]
        case_insensitive: bool,
    },
}

/// A rule's id and declared `ignores:`, the only fields the exclusions need.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct RuleIgnores {
    /// The rule id.
    pub id: String,
    /// The rule's `ignores:` entries, absent when it declares none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ignores: Option<Vec<IgnoreGlob>>,
}

/// Every `(rule_id, globs)` pair the active rule set declares via `ignores:`, without linking the
/// ast-grep engine: pack rules first, then user rules from `rules_dirs`, a user rule replacing a
/// pack rule of the same id outright (one that declares no `ignores:` clears the entry).
///
/// A user rule file that does not parse drops every user rule's exclusions, as the engine's own
/// rule loading fails as a whole on it.
#[must_use]
pub fn default_path_ignores(rules_dirs: &[String], builtin_pack_enabled: bool) -> DefaultPathIgnores {
    let mut collected: DefaultPathIgnores = Vec::new();
    if builtin_pack_enabled {
        let pack: Vec<RuleIgnores> =
            serde_json::from_str(PACK_IGNORES_JSON).expect("data/astgrep_pack_ignores.json is valid");
        collect(&pack, &mut collected);
    }
    if !rules_dirs.is_empty() {
        match user_rule_ignores(rules_dirs) {
            Ok(user) => collect(&user, &mut collected),
            Err(error) => {
                tracing::debug!(%error, "cannot read user ast-grep rules for their declared path exclusions");
            }
        }
    }
    collected
}

fn user_rule_ignores(rules_dirs: &[String]) -> anyhow::Result<Vec<RuleIgnores>> {
    let mut rules = Vec::new();
    for dir in rules_dirs {
        for path in collect_rule_paths(Path::new(dir)) {
            let yaml = fs::read_to_string(&path)?;
            let documents: Vec<RuleIgnores> =
                serde_saphyr::from_multiple(&yaml).map_err(|error| anyhow::anyhow!("{}: {error}", path.display()))?;
            rules.extend(documents);
        }
    }
    Ok(rules)
}

fn collect(rules: &[RuleIgnores], out: &mut DefaultPathIgnores) {
    for rule in rules {
        let globs = build_globs(&rule.id, rule.ignores.as_deref().unwrap_or_default());
        match out.iter_mut().find(|(id, _)| *id == rule.id) {
            Some(existing) => existing.1 = globs,
            None if globs.is_empty() => {}
            None => out.push((rule.id.clone(), globs)),
        }
    }
}

fn build_globs(rule_id: &str, entries: &[IgnoreGlob]) -> Vec<globset::Glob> {
    entries
        .iter()
        .filter_map(|entry| {
            let (glob, case_insensitive) = match entry {
                IgnoreGlob::Plain(glob) => (glob.as_str(), false),
                IgnoreGlob::Config { glob, case_insensitive } => (glob.as_str(), *case_insensitive),
            };
            globset::GlobBuilder::new(glob)
                .case_insensitive(case_insensitive)
                .build()
                .inspect_err(
                    |error| tracing::warn!(%rule_id, %glob, %error, "skipping unparseable `ignores:` glob in an ast-grep rule"),
                )
                .ok()
        })
        .collect()
}
