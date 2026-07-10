//! Native typos-cli configuration resolution.
//!
//! Reads `_typos.toml` / `.typos.toml` and `pyproject.toml`
//! `[tool.typos]` / `[tool.codespell]` sections, and merges the full ancestor
//! chain (nearest wins for maps, unioned for lists) to match typos-cli's
//! directory-tree merge semantics.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Parsed content of a native typos configuration source: `_typos.toml`,
/// `.typos.toml`, or a `pyproject.toml` `[tool.typos]` / `[tool.codespell]`
/// section.
///
/// All sections are optional; absent sections default to empty. Unknown keys
/// are silently ignored to match typos-cli's lenient parse semantics.
#[derive(Debug, Clone, Default)]
pub struct TyposNative {
    /// `[default.extend-words]` — word tokens whose keys are treated as valid spellings.
    pub extend_words: BTreeMap<String, String>,
    /// `[default.extend-identifiers]` — identifier tokens whose keys are treated as valid.
    pub extend_identifiers: BTreeMap<String, String>,
    /// `[files] extend-exclude` — gitignore-style globs of files to skip spell-checking.
    pub extend_exclude: Vec<String>,
    /// Flat list of words treated as valid. Sourced from the non-standard
    /// `[default].extend-ignore-words` key and from `pyproject.toml`
    /// `[tool.codespell] ignore-words-list` (comma-separated).
    pub extend_ignore_words: Vec<String>,
    /// `[default].extend-ignore-re` — regexes whose matched regions of a file are
    /// skipped entirely (e.g. to ignore base64 blobs or license headers).
    pub extend_ignore_re: Vec<String>,
    /// `[default].extend-ignore-words-re` — regexes; a word matching any of them is ignored.
    pub extend_ignore_words_re: Vec<String>,
    /// `[default].extend-ignore-identifiers-re` — regexes; an identifier matching any is ignored.
    pub extend_ignore_identifiers_re: Vec<String>,
}

/// Native typos-cli config file names in preference order.
const TYPOS_NATIVE_FILE_NAMES: [&str; 2] = ["_typos.toml", ".typos.toml"];

/// Resolve the effective native typos configuration for `dir` by merging the
/// full ancestor chain of typos config sources (nearest wins), matching
/// typos-cli's directory-tree merge semantics.
///
/// Each directory from `dir` up to the filesystem root contributes at most one
/// `_typos.toml`/`.typos.toml` file plus a `pyproject.toml` `[tool.typos]` /
/// `[tool.codespell]` section when present. Map entries (`extend-words`,
/// `extend-identifiers`) from a nearer config override farther ones; list
/// entries (`extend-exclude`, the `extend-ignore-*` regexes, ignore-words) are
/// unioned across the whole chain.
pub(crate) fn resolve_typos_native(dir: &Path) -> TyposNative {
    let sources = typos_config_sources(dir);
    let mut merged = TyposNative::default();
    for path in sources.into_iter().rev() {
        let parsed = parse_typos_native(&path);
        merged.extend_words.extend(parsed.extend_words);
        merged.extend_identifiers.extend(parsed.extend_identifiers);
        merged.extend_exclude.extend(parsed.extend_exclude);
        merged.extend_ignore_words.extend(parsed.extend_ignore_words);
        merged.extend_ignore_re.extend(parsed.extend_ignore_re);
        merged.extend_ignore_words_re.extend(parsed.extend_ignore_words_re);
        merged
            .extend_ignore_identifiers_re
            .extend(parsed.extend_ignore_identifiers_re);
    }
    merged
}

/// Collect the typos config sources for `dir` and its ancestors, nearest first.
/// A `pyproject.toml` is included only when it carries a `[tool.typos]` or
/// `[tool.codespell]` section, so unrelated manifests are skipped.
fn typos_config_sources(dir: &Path) -> Vec<PathBuf> {
    let mut sources = Vec::new();
    let mut current = Some(dir);
    while let Some(d) = current {
        for name in TYPOS_NATIVE_FILE_NAMES {
            let candidate = d.join(name);
            if candidate.is_file() {
                sources.push(candidate);
                break;
            }
        }
        let pyproject = d.join("pyproject.toml");
        if pyproject.is_file() && pyproject_has_typos_config(&pyproject) {
            sources.push(pyproject);
        }
        current = d.parent();
    }
    sources
}

/// Whether a `pyproject.toml` declares a `[tool.typos]` or `[tool.codespell]`
/// section (a cheap pre-check before the full parse).
fn pyproject_has_typos_config(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(table): Result<toml::Table, _> = toml::from_str(&text) else {
        return false;
    };
    table
        .get("tool")
        .and_then(|v| v.as_table())
        .is_some_and(|tool| tool.contains_key("typos") || tool.contains_key("codespell"))
}

/// Parse a native typos config source into a [`TyposNative`] value.
///
/// Handles `_typos.toml` / `.typos.toml` (config at the document root) and
/// `pyproject.toml` (config under `[tool.typos]`, plus `[tool.codespell]`).
/// Unknown keys and malformed sections are silently ignored (lenient parse).
/// Returns the default (empty) value on any I/O or parse error.
fn parse_typos_native(path: &Path) -> TyposNative {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return TyposNative::default(),
    };
    let table: toml::Table = match toml::from_str(&text) {
        Ok(t) => t,
        Err(_) => return TyposNative::default(),
    };

    let mut result = TyposNative::default();
    let is_pyproject = path.file_name().is_some_and(|n| n == "pyproject.toml");

    let typos_root: Option<&toml::Table> = if is_pyproject {
        table
            .get("tool")
            .and_then(|v| v.as_table())
            .and_then(|tool| tool.get("typos"))
            .and_then(|v| v.as_table())
    } else {
        Some(&table)
    };

    if let Some(root) = typos_root {
        collect_typos_default(root.get("default").and_then(|v| v.as_table()), &mut result);
        if let Some(files) = root.get("files").and_then(|v| v.as_table())
            && let Some(excl) = files.get("extend-exclude").and_then(|v| v.as_array())
        {
            result.extend_exclude = string_array(excl);
        }
    }

    if is_pyproject
        && let Some(codespell) = table
            .get("tool")
            .and_then(|v| v.as_table())
            .and_then(|tool| tool.get("codespell"))
            .and_then(|v| v.as_table())
        && let Some(list) = codespell.get("ignore-words-list").and_then(|v| v.as_str())
    {
        result.extend_ignore_words.extend(
            list.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
        );
    }

    result
}

/// Populate a [`TyposNative`] from a `[default]` table (typos-cli schema).
fn collect_typos_default(default: Option<&toml::Table>, out: &mut TyposNative) {
    let Some(default) = default else { return };
    if let Some(words) = default.get("extend-words").and_then(|v| v.as_table()) {
        for (k, v) in words {
            if let Some(val) = v.as_str() {
                out.extend_words.insert(k.clone(), val.to_string());
            }
        }
    }
    if let Some(idents) = default.get("extend-identifiers").and_then(|v| v.as_table()) {
        for (k, v) in idents {
            if let Some(val) = v.as_str() {
                out.extend_identifiers.insert(k.clone(), val.to_string());
            }
        }
    }
    if let Some(arr) = default.get("extend-ignore-words").and_then(|v| v.as_array()) {
        out.extend_ignore_words.extend(string_array(arr));
    }
    if let Some(arr) = default.get("extend-ignore-re").and_then(|v| v.as_array()) {
        out.extend_ignore_re.extend(string_array(arr));
    }
    if let Some(arr) = default.get("extend-ignore-words-re").and_then(|v| v.as_array()) {
        out.extend_ignore_words_re.extend(string_array(arr));
    }
    if let Some(arr) = default.get("extend-ignore-identifiers-re").and_then(|v| v.as_array()) {
        out.extend_ignore_identifiers_re.extend(string_array(arr));
    }
}

/// Collect the string elements of a TOML array, dropping non-string entries.
fn string_array(arr: &[toml::Value]) -> Vec<String> {
    arr.iter().filter_map(|v| v.as_str()).map(str::to_string).collect()
}
