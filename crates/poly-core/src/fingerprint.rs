//! A stable digest of the configuration a run was governed by.
//!
//! Two runs of the same binary over the same tree can enforce different rules:
//! a `poly.toml`, a `poly.local.toml`, a nested config or an `extends` base can
//! change underneath an identical executable. Both runs report clean, and
//! nothing in either says they are not comparable — so a consumer memoizing
//! results on the binary's identity alone is wrong the moment config moves
//! without a version bump.
//!
//! A run can be governed by **several** configs (ADR 0018), so this is a list,
//! one entry per config that governed at least one discovered file, each naming
//! the directory it resolved from. In a monorepo two sibling packages legitimately
//! carry different hashes, and the root is what lets a caller attribute the
//! difference to a package rather than treat it as an anomaly.

use std::path::Path;

use serde::Serialize;

use crate::resolve::ConfigSet;

/// Schema version of the hash input framing, folded in so a change to *what* is
/// hashed cannot be mistaken for a change to the configuration itself.
const FINGERPRINT_VERSION: &str = "1";

/// The identity of one resolved configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct ConfigFingerprint {
    /// Directory the config resolved from, relative to the run's root, or
    /// `null` when the caller passed an explicit `--config` (which bypasses the
    /// directory cascade and governs every file).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    /// Digest of the fully merged, normalized config — `extends` bases, the
    /// directory cascade and `poly.local.toml` all applied.
    ///
    /// Machine-independent by construction: absolute paths the resolver
    /// introduces are made relative before hashing, so the same commit hashes
    /// the same on CI and on a laptop.
    pub hash: String,
}

/// Fingerprint every config that governed a file in this run, indexed by the
/// `config_id` a result carries.
///
/// Computed once per run, after resolution and before the file loop.
pub(crate) fn fingerprints(
    configs: &ConfigSet,
    resolver: &dyn poly_config::BaseConfigResolver,
) -> Vec<ConfigFingerprint> {
    (0..configs.len())
        .map(|id| {
            let dir = configs.config_dir(id);
            let hash = dir.map_or_else(
                // No directory means an explicit `--config`: the cascade was
                // bypassed, so there is no directory to resolve a table from and
                // nothing honest to hash.
                || "unresolved".to_owned(),
                |dir| match poly_config::PolyConfig::effective_table_with(dir, resolver) {
                    Ok(table) => hash_table(&table, dir),
                    // A config that failed to re-resolve is reported as unknown
                    // rather than as some other config's hash.
                    Err(_) => "unresolved".to_owned(),
                },
            );
            ConfigFingerprint {
                root: dir.map(|dir| relative_to_run(dir, configs)),
                hash,
            }
        })
        .collect()
}

/// Digest one merged config table.
///
/// `rules.dirs` is rewritten relative to the config root first. The resolver
/// stores those as absolute paths, so hashing them verbatim would make the
/// digest depend on where the repository happens to be checked out — two
/// identical commits would never agree.
fn hash_table(table: &toml::Table, dir: &Path) -> String {
    let mut table = table.clone();
    relativize_rule_dirs(&mut table, dir);
    let serialized = toml::to_string(&table).unwrap_or_default();
    let mut hasher = blake3::Hasher::new();
    hasher.update(FINGERPRINT_VERSION.as_bytes());
    hasher.update(b"\0");
    hasher.update(serialized.as_bytes());
    format!("{FINGERPRINT_VERSION}/{}", &hasher.finalize().to_hex()[..16])
}

/// Rewrite `[rules] dirs` entries as paths relative to `dir`, leaving anything
/// already relative alone.
fn relativize_rule_dirs(table: &mut toml::Table, dir: &Path) {
    let Some(dirs) = table
        .get_mut("rules")
        .and_then(toml::Value::as_table_mut)
        .and_then(|rules| rules.get_mut("dirs"))
        .and_then(toml::Value::as_array_mut)
    else {
        return;
    };
    for entry in dirs {
        let Some(path) = entry.as_str() else { continue };
        let relative = Path::new(path)
            .strip_prefix(dir)
            .map_or_else(|_| path.to_owned(), |rest| rest.display().to_string());
        *entry = toml::Value::String(relative);
    }
}

/// A config directory as a path relative to the run's anchor, so the reported
/// root is portable between checkouts rather than naming someone's home
/// directory.
///
/// Both sides are canonicalized before stripping: on macOS a discovered path and
/// a resolved one routinely differ by the `/private` prefix, and a plain
/// `strip_prefix` between them silently fails and leaks the absolute path.
fn relative_to_run(dir: &Path, configs: &ConfigSet) -> String {
    let Some(anchor) = configs.root_dir().or_else(|| configs.config_dir(0)) else {
        return dir.display().to_string();
    };
    let canonical = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    canonical(dir)
        .strip_prefix(canonical(anchor))
        .map_or_else(|_| dir.display().to_string(), |rest| rest.display().to_string())
}
