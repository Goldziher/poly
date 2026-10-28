//! The project's own oxlint configuration (`.oxlintrc.json[c]`, root and nested).
//!
//! A repository that already configures oxlint gets exactly its rules, categories, plugins,
//! overrides and `ignorePatterns`, read with `oxc_linter`'s own `Oxlintrc`. Like oxlint, the
//! nearest config above a file wins and does not inherit from the root unless it says `extends`.
//!
//! Configs are found by walking *up* from each linted file, never by walking the tree: a
//! single-file run (a hook) must not pay for a scan of the repository.
//!
//! What cannot run in-process is reported once with a warning instead of being dropped silently:
//! `jsPlugins` are JavaScript modules that need a JS runtime, so their rules are not applied.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use oxc_linter::Oxlintrc;
use poly_config::OxlintNative;

use crate::language::Language;
use crate::path_ignore::PathIgnore;

const FILE_NAMES: [&str; 2] = [".oxlintrc.json", ".oxlintrc.jsonc"];

/// The oxlint configs that govern one directory.
#[derive(Debug)]
pub(crate) struct Governing {
    /// The innermost config: the one whose rules apply.
    pub nearest: PathBuf,
    /// Content digest of every config from the root down to `nearest`.
    pub digest: String,
    /// `ignorePatterns` of every config in the chain, each relative to its own directory.
    pub ignore: Option<PathIgnore>,
}

fn config_in(dir: &Path) -> Option<PathBuf> {
    FILE_NAMES.iter().map(|name| dir.join(name)).find(|path| path.is_file())
}

/// The configs governing a file in `dir`, ancestors of `dir` up to the root config's directory.
/// Memoized per directory, so a tree of files costs one lookup per directory.
pub(crate) fn governing(root: &Path, dir: &Path) -> Arc<Governing> {
    type Key = (PathBuf, PathBuf);
    static MEMO: OnceLock<Mutex<HashMap<Key, Arc<Governing>>>> = OnceLock::new();
    let dir = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    let key = (root.to_path_buf(), dir.clone());
    if let Some(found) = MEMO
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&key)
    {
        return Arc::clone(found);
    }
    let computed = Arc::new(compute(root, &dir));
    MEMO.get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(key, Arc::clone(&computed));
    computed
}

fn compute(root: &Path, dir: &Path) -> Governing {
    let root_dir = root.parent().unwrap_or(Path::new(""));
    let mut chain: Vec<PathBuf> = dir
        .ancestors()
        .take_while(|ancestor| ancestor.starts_with(root_dir))
        .filter_map(config_in)
        .filter(|found| found != root)
        .collect();
    chain.push(root.to_path_buf());
    chain.reverse();

    let mut hasher = blake3::Hasher::new();
    let mut ignores = Vec::new();
    for file in &chain {
        hasher.update(file.as_os_str().as_encoded_bytes());
        hasher.update(&std::fs::read(file).unwrap_or_default());
        if let (Some(parent), Ok(config)) = (file.parent(), Oxlintrc::from_file(file)) {
            ignores.push((parent.to_path_buf(), config.ignore_patterns.clone()));
        }
    }
    Governing {
        nearest: chain.last().cloned().unwrap_or_else(|| root.to_path_buf()),
        digest: hasher.finalize().to_hex().to_string(),
        ignore: PathIgnore::new(&ignores),
    }
}

/// Report a config's `jsPlugins` once: they are JavaScript modules, which poly's in-process
/// linter has no runtime to execute, so the rules they define are not applied.
pub(crate) fn warn_about_js_plugins(file: &Path, config: &Oxlintrc) {
    let mut specifiers: Vec<&str> = config
        .external_plugins
        .iter()
        .flatten()
        .map(|entry| entry.specifier.as_str())
        .collect();
    specifiers.sort_unstable();
    if !specifiers.is_empty() {
        tracing::warn!(
            "oxlint jsPlugins in {} are not run: they are JavaScript modules and poly lints in-process \
             without a JS runtime, so the rules they define are skipped ({}). Run oxlint for those rules.",
            file.display(),
            specifiers.join(", ")
        );
    }
}

fn is_lintable(language: &Language) -> bool {
    matches!(
        language,
        Language::JavaScript | Language::TypeScript | Language::Jsx | Language::Tsx
    )
}

/// `[lint.<lang>.oxc]` defaults naming the project's root oxlint config for JS/TS.
pub(crate) fn engine_defaults(native: &OxlintNative, language: &Language, engine: &str) -> toml::Table {
    let mut table = toml::Table::new();
    if let Some(file) = native
        .file
        .as_deref()
        .filter(|_| engine == "oxc" && is_lintable(language))
    {
        table.insert(
            "oxlintrc".to_owned(),
            toml::Value::String(file.to_string_lossy().into_owned()),
        );
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_nearest_config_wins_and_every_config_in_the_chain_is_digested() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path().join(".oxlintrc.json");
        std::fs::write(&root, r#"{"ignorePatterns": ["**/dist/**"]}"#).unwrap();
        let nested_dir = repo.path().join("pkg");
        std::fs::create_dir_all(nested_dir.join("src")).unwrap();
        let nested = nested_dir.join(".oxlintrc.jsonc");
        std::fs::write(&nested, r#"{ /* c */ "ignorePatterns": ["gen.ts"] }"#).unwrap();

        let outside = governing(&root, repo.path());
        assert_eq!(outside.nearest, root);
        let inside = governing(&root, &nested_dir.join("src"));
        assert_eq!(inside.nearest, nested);
        assert_ne!(inside.digest, outside.digest, "the nested config is part of the digest");

        let ignore = inside.ignore.as_ref().expect("both configs carry patterns");
        assert!(ignore.is_ignored(&repo.path().join("a/dist/x.js")));
        assert!(ignore.is_ignored(&nested_dir.join("gen.ts")));
        assert!(!ignore.is_ignored(&repo.path().join("other/gen.ts")));
    }

    #[test]
    fn only_js_and_ts_get_the_config() {
        let native = OxlintNative {
            file: Some(PathBuf::from("/repo/.oxlintrc.json")),
        };
        assert!(engine_defaults(&native, &Language::TypeScript, "oxc").contains_key("oxlintrc"));
        assert!(engine_defaults(&native, &Language::Json, "oxc").is_empty());
        assert!(engine_defaults(&native, &Language::Python, "ruff").is_empty());
    }
}
