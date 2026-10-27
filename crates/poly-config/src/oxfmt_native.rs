//! Native `oxfmt` configuration resolution.
//!
//! Reads the nearest `.oxfmtrc.json` / `.oxfmtrc.jsonc` (JSON with comments and
//! trailing commas) so poly's oxfmt-equivalent formatters can honor the project's
//! existing formatter configuration instead of silently using their own defaults.
//! The raw values are kept as JSON; `poly-core` translates them into the option
//! keys each backend understands.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

/// Native oxfmt config file names in preference order.
const OXFMT_FILE_NAMES: [&str; 2] = [".oxfmtrc.json", ".oxfmtrc.jsonc"];

/// One entry of the `overrides` array: options applied to files matching `files`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OxfmtOverride {
    /// `files` globs the override applies to.
    pub files: Vec<String>,
    /// `excludeFiles` globs removed from the override.
    pub exclude_files: Vec<String>,
    /// The formatting options the override sets.
    pub options: Map<String, Value>,
}

/// Parsed content of a native oxfmt configuration file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OxfmtNative {
    /// The config file, when one was found.
    pub file: Option<PathBuf>,
    /// Top-level formatting options (`printWidth`, `tabWidth`, `sortImports`, ...).
    pub options: Map<String, Value>,
    /// `overrides` entries in file order; later entries win.
    pub overrides: Vec<OxfmtOverride>,
    /// `ignorePatterns` gitignore-style globs, relative to the config file's directory.
    pub ignore_patterns: Vec<String>,
    /// Why the file could not be used, when it was found but unreadable or malformed.
    pub error: Option<String>,
}

impl OxfmtNative {
    /// The directory `ignorePatterns` and override globs are relative to.
    pub fn dir(&self) -> Option<&Path> {
        self.file.as_deref().and_then(Path::parent)
    }

    /// Whether a config file was found and parsed.
    pub fn is_present(&self) -> bool {
        self.file.is_some() && self.error.is_none()
    }
}

/// Resolve the native oxfmt configuration governing `dir`: the nearest config file
/// walking upward, bounded at the enclosing git root.
pub(crate) fn resolve_oxfmt_native(dir: &Path) -> OxfmtNative {
    let Some(file) = find_oxfmt_file(dir) else {
        return OxfmtNative::default();
    };
    match std::fs::read_to_string(&file) {
        Ok(text) => parse(&file, &text),
        Err(error) => OxfmtNative {
            error: Some(format!("cannot read {}: {error}", file.display())),
            file: Some(file),
            ..OxfmtNative::default()
        },
    }
}

fn find_oxfmt_file(dir: &Path) -> Option<PathBuf> {
    let dir = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    let mut current = Some(dir.as_path());
    while let Some(candidate) = current {
        for name in OXFMT_FILE_NAMES {
            let path = candidate.join(name);
            if path.is_file() {
                return Some(path);
            }
        }
        if candidate.join(".git").exists() {
            return None;
        }
        current = candidate.parent();
    }
    None
}

fn parse(file: &Path, text: &str) -> OxfmtNative {
    let value: Value = match json5::from_str(text) {
        Ok(value) => value,
        Err(error) => {
            return OxfmtNative {
                file: Some(file.to_path_buf()),
                error: Some(format!("cannot parse {}: {error}", file.display())),
                ..OxfmtNative::default()
            };
        }
    };
    let Value::Object(mut options) = value else {
        return OxfmtNative {
            file: Some(file.to_path_buf()),
            error: Some(format!("{} is not a JSON object", file.display())),
            ..OxfmtNative::default()
        };
    };
    options.remove("$schema");
    let ignore_patterns = string_list(options.remove("ignorePatterns"));
    let overrides = match options.remove("overrides") {
        Some(Value::Array(entries)) => entries.into_iter().filter_map(parse_override).collect(),
        _ => Vec::new(),
    };
    OxfmtNative {
        file: Some(file.to_path_buf()),
        options,
        overrides,
        ignore_patterns,
        error: None,
    }
}

fn parse_override(entry: Value) -> Option<OxfmtOverride> {
    let Value::Object(mut entry) = entry else {
        return None;
    };
    let options = match entry.remove("options") {
        Some(Value::Object(options)) => options,
        _ => Map::new(),
    };
    Some(OxfmtOverride {
        files: string_list(entry.remove("files")),
        exclude_files: string_list(entry.remove("excludeFiles")),
        options,
    })
}

fn string_list(value: Option<Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => items
            .into_iter()
            .filter_map(|item| item.as_str().map(str::to_owned))
            .collect(),
        Some(Value::String(single)) => vec![single],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_jsonc_with_comments_and_trailing_commas() {
        let native = parse(
            Path::new("/p/.oxfmtrc.jsonc"),
            r#"// c
            { "$schema": "x", "tabWidth": 4, /* c */ "overrides": [{"files": ["**/*.json"], "options": {"tabWidth": 2,},},], "ignorePatterns": ["**/dist/**",], }"#,
        );
        assert!(native.is_present(), "{:?}", native.error);
        assert_eq!(native.options.get("tabWidth"), Some(&Value::from(4)));
        assert!(!native.options.contains_key("$schema"));
        assert_eq!(native.overrides.len(), 1);
        assert_eq!(native.overrides[0].files, ["**/*.json"]);
        assert_eq!(native.ignore_patterns, ["**/dist/**"]);
        assert_eq!(native.dir(), Some(Path::new("/p")));
    }

    #[test]
    fn malformed_file_records_an_error() {
        let native = parse(Path::new("/p/.oxfmtrc.json"), "{ nope");
        assert!(!native.is_present());
        assert!(native.error.is_some());
    }

    #[test]
    fn discovery_stops_at_the_git_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".oxfmtrc.json"), "{}").unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("sub")).unwrap();
        assert_eq!(resolve_oxfmt_native(&repo.join("sub")), OxfmtNative::default());
        std::fs::write(repo.join(".oxfmtrc.jsonc"), r#"{"semi": false}"#).unwrap();
        let native = resolve_oxfmt_native(&repo.join("sub"));
        assert!(native.is_present());
        assert_eq!(native.options.get("semi"), Some(&Value::Bool(false)));
    }
}
