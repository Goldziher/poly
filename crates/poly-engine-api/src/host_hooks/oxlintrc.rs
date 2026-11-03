//! The project's own oxlint configuration (`.oxlintrc.json[c]`, root and nested), as far as the
//! host needs it: which config governs a file, a digest of the chain for the cache key, and the
//! `ignorePatterns` that exclude files.
//!
//! The rules themselves are the engine's business (`oxc_linter::Oxlintrc` in the oxc component).
//! The host reads only the two things the cache and the planner need, with a small JSONC reader,
//! so it never links oxc. Like oxlint, the nearest config above a file wins and does not inherit
//! from the root unless it says `extends`.
//!
//! Configs are found by walking *up* from each linted file, never by walking the tree: a
//! single-file run (a hook) must not pay for a scan of the repository.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use poly_config::OxlintNative;
use serde_json::Value;

use super::path_ignore::PathIgnore;
use crate::config::EngineConfig;
use crate::language::Language;

/// Key of the root config's path in an oxc engine's lint table; set by poly.
pub const RC_KEY: &str = "oxlintrc";

const FILE_NAMES: [&str; 2] = [".oxlintrc.json", ".oxlintrc.jsonc"];

/// The oxlint configs that govern one directory.
#[derive(Debug)]
pub struct Governing {
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

/// The `ignorePatterns` of the oxlint config at `file`, or `None` when it cannot be read as
/// JSONC (comments and trailing commas are accepted, as oxlint accepts them) or when the key is
/// not a list of strings, which oxlint rejects too. A config without the key has no patterns.
pub fn ignore_patterns(file: &Path) -> Option<Vec<String>> {
    let text = std::fs::read_to_string(file).ok()?;
    ignore_patterns_of(&text)
}

fn ignore_patterns_of(text: &str) -> Option<Vec<String>> {
    let Value::Object(mut root) = json5::from_str::<Value>(text).ok()? else {
        return None;
    };
    match root.remove("ignorePatterns") {
        None | Some(Value::Null) => Some(Vec::new()),
        Some(Value::Array(items)) => items
            .into_iter()
            .map(|item| match item {
                Value::String(pattern) => Some(pattern),
                _ => None,
            })
            .collect(),
        Some(_) => None,
    }
}

/// The configs governing a file in `dir`, ancestors of `dir` up to the root config's directory.
/// Memoized per directory, so a tree of files costs one lookup per directory.
pub fn governing(root: &Path, dir: &Path) -> Arc<Governing> {
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
        if let (Some(parent), Some(patterns)) = (file.parent(), ignore_patterns(file)) {
            ignores.push((parent.to_path_buf(), patterns));
        }
    }
    Governing {
        nearest: chain.last().cloned().unwrap_or_else(|| root.to_path_buf()),
        digest: hasher.finalize().to_hex().to_string(),
        ignore: PathIgnore::new(&ignores),
    }
}

/// The project's root oxlint config named by `cfg`, if any.
pub fn root_of(cfg: &EngineConfig) -> Option<&Path> {
    match cfg.options.get("oxlintrc") {
        Some(toml::Value::String(path)) => Some(Path::new(path)),
        // `oxlintrc = false` opts out of the project file; `true` keeps discovery's choice.
        Some(toml::Value::Boolean(_)) => None,
        _ => None,
    }
}

/// The configs governing `path`, or `None` when the project has no oxlint config for this table.
pub fn governing_for(cfg: &EngineConfig, path: &Path) -> Option<Arc<Governing>> {
    let root = root_of(cfg)?;
    Some(governing(root, path.parent().unwrap_or(Path::new(""))))
}

fn is_lintable(language: &Language) -> bool {
    matches!(
        language,
        Language::JavaScript | Language::TypeScript | Language::Jsx | Language::Tsx
    )
}

/// `[lint.<lang>.oxc]` defaults naming the project's root oxlint config for JS/TS.
pub fn engine_defaults(native: &OxlintNative, language: &Language, engine: &str) -> toml::Table {
    let mut table = toml::Table::new();
    if let Some(file) = native
        .file
        .as_deref()
        .filter(|_| engine == "oxc" && is_lintable(language))
    {
        table.insert(
            RC_KEY.to_owned(),
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

    #[test]
    fn the_reader_accepts_comments_and_trailing_commas() {
        let text = r#"
        // line comment
        {
            /* block */ "rules": { "no-console": "warn", },
            "ignorePatterns": [
                "**/dist/**", // trailing comment
                "gen.ts",
            ],
        }"#;
        assert_eq!(
            ignore_patterns_of(text),
            Some(vec!["**/dist/**".to_owned(), "gen.ts".to_owned()])
        );
    }

    #[test]
    fn a_comment_marker_inside_a_string_is_not_a_comment() {
        let text = r#"{"ignorePatterns": ["http://x/*.js", "a/*b*/c"]}"#;
        assert_eq!(
            ignore_patterns_of(text),
            Some(vec!["http://x/*.js".to_owned(), "a/*b*/c".to_owned()])
        );
    }

    #[test]
    fn a_config_without_ignore_patterns_has_none_and_a_malformed_one_is_unreadable() {
        assert_eq!(ignore_patterns_of(r#"{"rules": {}}"#), Some(Vec::new()));
        assert_eq!(ignore_patterns_of(r#"{"ignorePatterns": null}"#), Some(Vec::new()));
        assert_eq!(ignore_patterns_of(r#"{"ignorePatterns": "dist"}"#), None);
        assert_eq!(ignore_patterns_of(r#"{"ignorePatterns": ["a", 1]}"#), None);
        assert_eq!(ignore_patterns_of("[1, 2]"), None);
        assert_eq!(ignore_patterns_of("{ not json"), None);
    }

    #[test]
    fn the_root_comes_from_the_options_table() {
        let mut cfg = EngineConfig {
            globals: crate::config::GlobalDefaults::default(),
            indent_width: 2,
            options: toml::Table::new(),
        };
        assert!(root_of(&cfg).is_none());
        cfg.options.insert(RC_KEY.to_owned(), toml::Value::Boolean(false));
        assert!(root_of(&cfg).is_none());
        cfg.options
            .insert(RC_KEY.to_owned(), toml::Value::String("/r/.oxlintrc.json".into()));
        assert_eq!(root_of(&cfg), Some(Path::new("/r/.oxlintrc.json")));
    }
}
