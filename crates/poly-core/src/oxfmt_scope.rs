//! The `.oxfmtrc.json[c]` that governs one file, and the options it gives that file.
//!
//! [`crate::oxfmt_native`] turns the project's config into per-language tables. oxfmt, though,
//! decides per file: the nearest config above a file replaces the root one, and an
//! `overrides[].files` glob (relative to the config's directory) changes the options of the files
//! it matches. This module resolves both at format time, so a directory-scoped override reaches
//! exactly its files, and gives the formatting cache an input that changes with the governing
//! config, since the content key alone cannot tell two paths apart.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use poly_config::{OxfmtNative, OxfmtOverride, resolve_oxfmt_native};

use crate::config::EngineConfig;
use crate::engine::SourceFile;
use crate::language::Language;

/// Key of the root config's path in an oxfmt-equivalent engine's table; set by poly.
pub(crate) const RC_KEY: &str = "oxfmtrc";

struct Scope {
    native: OxfmtNative,
    digest: String,
    include: Vec<GlobSet>,
    exclude: Vec<GlobSet>,
}

impl Scope {
    fn new(native: OxfmtNative) -> Self {
        let digest = native
            .file
            .as_ref()
            .and_then(|file| std::fs::read(file).ok())
            .map(|bytes| blake3::hash(&bytes).to_hex().to_string())
            .unwrap_or_default();
        let include = native.overrides.iter().map(|entry| compile(&entry.files)).collect();
        let exclude = native.overrides.iter().map(|entry| compile(&entry.exclude_files)).collect();
        Scope {
            native,
            digest,
            include,
            exclude,
        }
    }

    /// Indexes of the overrides that apply to `path`, in config order.
    fn matching(&self, path: &Path) -> Vec<usize> {
        let Some(root) = self.native.dir() else {
            return Vec::new();
        };
        let relative = path.strip_prefix(root).unwrap_or(path);
        (0..self.include.len())
            .filter(|&index| self.include[index].is_match(relative) && !self.exclude[index].is_match(relative))
            .collect()
    }
}

/// Globs as oxfmt reads them: relative to the config's directory, `*` stays inside a path
/// segment, and a pattern with no `/` matches at any depth unless it starts with `./`.
fn compile(patterns: &[String]) -> GlobSet {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let (bare, anchored) = match pattern.strip_prefix("./") {
            Some(rest) => (rest, true),
            None => (pattern.as_str(), false),
        };
        let normalized = if anchored || bare.contains('/') {
            bare.to_owned()
        } else {
            format!("**/{bare}")
        };
        if let Ok(glob) = GlobBuilder::new(&normalized).literal_separator(true).build() {
            builder.add(glob);
        }
    }
    builder.build().unwrap_or_else(|_| GlobSet::empty())
}

/// The scope of the nearest oxfmt config at or above `dir`, memoized per directory so a tree of
/// files costs a couple of `stat`s per directory and one parse per config.
fn scope_for(dir: &Path) -> Arc<Scope> {
    static MEMO: OnceLock<Mutex<HashMap<PathBuf, Arc<Scope>>>> = OnceLock::new();
    let memo = MEMO.get_or_init(Mutex::default);
    let lock = || memo.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(found) = lock().get(dir) {
        return Arc::clone(found);
    }
    let has_config = [".oxfmtrc.json", ".oxfmtrc.jsonc"]
        .iter()
        .any(|name| dir.join(name).is_file());
    let scope = match dir.parent() {
        Some(parent) if !has_config && !dir.join(".git").exists() => scope_for(parent),
        _ => Arc::new(Scope::new(resolve_oxfmt_native(dir))),
    };
    lock().insert(dir.to_path_buf(), Arc::clone(&scope));
    scope
}

fn scope_of(path: &Path) -> Option<(Arc<Scope>, PathBuf)> {
    let absolute = std::path::absolute(path).ok()?;
    let scope = scope_for(absolute.parent()?);
    scope.native.is_present().then_some((scope, absolute))
}

/// `cfg` as it applies to `src`: the options of the nearest oxfmt config when it is not the root
/// one the table was built from, with every matching `overrides` entry laid over them.
/// `cfg` itself when neither changes anything.
pub(crate) fn path_config<'a>(cfg: &'a EngineConfig, src: &SourceFile, engine: &str) -> Cow<'a, EngineConfig> {
    let Some(root_rc) = cfg.options.get(RC_KEY).and_then(toml::Value::as_str) else {
        return Cow::Borrowed(cfg);
    };
    let Some((scope, path)) = scope_of(&src.path) else {
        return Cow::Borrowed(cfg);
    };
    let matched: Vec<&OxfmtOverride> = scope
        .matching(&path)
        .into_iter()
        .map(|index| &scope.native.overrides[index])
        .collect();
    let nested = scope.native.file.as_deref() != Some(Path::new(root_rc));
    if !nested && matched.is_empty() {
        return Cow::Borrowed(cfg);
    }
    let Some(layer) = crate::oxfmt_native::path_table(&scope.native, &src.language, engine, &matched, nested) else {
        return Cow::Borrowed(cfg);
    };
    let mut options = cfg.options.clone();
    if nested {
        for key in root_keys(root_rc, &src.language, engine) {
            options.remove(&key);
        }
        if let Some(file) = &scope.native.file {
            options.insert(RC_KEY.to_owned(), toml::Value::String(file.to_string_lossy().into_owned()));
        }
    }
    options.extend(layer);
    let indent_width = options
        .get("indent_width")
        .and_then(toml::Value::as_integer)
        .and_then(|width| usize::try_from(width).ok())
        .filter(|&width| width > 0)
        .unwrap_or(cfg.indent_width);
    Cow::Owned(EngineConfig {
        globals: cfg.globals.clone(),
        indent_width,
        options,
    })
}

/// The keys the root config contributed to the engine's table, which a nested config replaces
/// instead of inheriting.
fn root_keys(root_rc: &str, language: &Language, engine: &str) -> Vec<String> {
    let Some(dir) = Path::new(root_rc).parent() else {
        return Vec::new();
    };
    let root = scope_for(dir);
    crate::oxfmt_native::engine_defaults(&root.native, language, engine)
        .into_iter()
        .map(|(key, _)| key)
        .collect()
}

/// What the formatted output of `path` depends on beyond its bytes and the engine's table: the
/// content of the governing config and which of its overrides match. `None` without an oxfmt
/// config, where the table says everything.
pub(crate) fn input_salt(cfg: &EngineConfig, path: &Path) -> Option<String> {
    cfg.options.get(RC_KEY)?;
    let (scope, absolute) = scope_of(path)?;
    let matched: Vec<String> = scope
        .matching(&absolute)
        .iter()
        .map(ToString::to_string)
        .collect();
    Some(format!("oxfmtrc:{}:{}", scope.digest, matched.join(",")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::GlobalDefaults;

    fn repo(root_rc: &str) -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir(repo.path().join(".git")).unwrap();
        std::fs::write(repo.path().join(".oxfmtrc.json"), root_rc).unwrap();
        repo
    }

    fn engine_cfg(repo: &Path, indent: i64) -> EngineConfig {
        let mut options = toml::Table::new();
        options.insert(RC_KEY.to_owned(), toml::Value::String(repo.join(".oxfmtrc.json").display().to_string()));
        options.insert("indent_width".to_owned(), toml::Value::Integer(indent));
        options.insert("line_width".to_owned(), toml::Value::Integer(100));
        EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: usize::try_from(indent).unwrap(),
            options,
        }
    }

    fn source(path: PathBuf) -> SourceFile {
        SourceFile {
            path,
            language: Language::TypeScript,
            content: "".into(),
        }
    }

    fn indent(cfg: &EngineConfig, path: PathBuf) -> usize {
        path_config(cfg, &source(path), "oxc").indent_width
    }

    #[test]
    fn a_directory_override_reaches_only_the_files_under_it() {
        let repo = repo(r#"{"tabWidth":4,"overrides":[{"files":["products/**"],"options":{"tabWidth":8}}]}"#);
        let cfg = engine_cfg(repo.path(), 4);
        assert_eq!(indent(&cfg, repo.path().join("products/em/a.ts")), 8);
        assert_eq!(indent(&cfg, repo.path().join("armis/a.ts")), 4);
        assert!(matches!(
            path_config(&cfg, &source(repo.path().join("armis/a.ts")), "oxc"),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn override_globs_follow_oxfmt_semantics() {
        let repo = repo(
            r#"{"tabWidth":4,"overrides":[
                {"files":["*.gen.ts"],"excludeFiles":["keep/**"],"options":{"tabWidth":2}},
                {"files":["./top.ts"],"options":{"tabWidth":6}}]}"#,
        );
        let cfg = engine_cfg(repo.path(), 4);
        assert_eq!(indent(&cfg, repo.path().join("deep/dir/a.gen.ts")), 2);
        assert_eq!(indent(&cfg, repo.path().join("keep/a.gen.ts")), 4);
        assert_eq!(indent(&cfg, repo.path().join("top.ts")), 6);
        assert_eq!(indent(&cfg, repo.path().join("sub/top.ts")), 4);
    }

    #[test]
    fn a_nested_config_replaces_the_root_one() {
        let repo = repo(r#"{"tabWidth":4,"singleQuote":true}"#);
        std::fs::create_dir_all(repo.path().join("pkg/src")).unwrap();
        std::fs::write(repo.path().join("pkg/.oxfmtrc.json"), r#"{"tabWidth":3}"#).unwrap();
        let mut cfg = engine_cfg(repo.path(), 4);
        cfg.options
            .insert("quote_style".to_owned(), toml::Value::String("single".to_owned()));
        let resolved = path_config(&cfg, &source(repo.path().join("pkg/src/a.ts")), "oxc");
        assert_eq!(resolved.indent_width, 3);
        assert!(!resolved.options.contains_key("quote_style"), "{:?}", resolved.options);
        let outside = path_config(&cfg, &source(repo.path().join("a.ts")), "oxc");
        assert_eq!(outside.options.get("quote_style").and_then(toml::Value::as_str), Some("single"));
    }

    #[test]
    fn the_salt_follows_the_governing_config_and_the_overrides_that_match() {
        let first = repo(r#"{"overrides":[{"files":["products/**"],"options":{"tabWidth":8}}]}"#);
        let second = repo(r#"{"overrides":[{"files":["products/**"],"options":{"tabWidth":2}}]}"#);
        let cfg_first = engine_cfg(first.path(), 4);
        let cfg_second = engine_cfg(second.path(), 4);
        let inside = |repo: &tempfile::TempDir, cfg: &EngineConfig| input_salt(cfg, &repo.path().join("products/a.ts"));
        let outside = |repo: &tempfile::TempDir, cfg: &EngineConfig| input_salt(cfg, &repo.path().join("armis/a.ts"));
        assert_ne!(inside(&first, &cfg_first), inside(&second, &cfg_second));
        assert_ne!(inside(&first, &cfg_first), outside(&first, &cfg_first));
        assert_eq!(input_salt(&EngineConfig { options: toml::Table::new(), ..cfg_first }, Path::new("a.ts")), None);
    }
}
