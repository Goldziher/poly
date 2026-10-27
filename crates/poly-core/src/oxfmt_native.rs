//! Translation of a native `.oxfmtrc.json[c]` ([`poly_config::OxfmtNative`]) into the option
//! keys poly's oxfmt-equivalent backends read, plus its `ignorePatterns`.
//!
//! The project's formatter config is the source of truth for the languages oxfmt owns, so a
//! repository that already ships one needs no duplicate `[fmt.*]` tables to get byte-identical
//! output. Everything derived here sits *under* the user's `poly.toml` tables: an explicit
//! `[fmt.<lang>.<engine>]` key always wins.
//!
//! What cannot be honored is reported once per process with a warning instead of being dropped
//! silently: options the Rust formatters do not implement (`sortTailwindcss`, whose class sorter
//! is JavaScript-side), and `overrides` scoped to part of a language (a directory rather than an
//! extension), because poly resolves formatter options per language, not per path.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use poly_config::OxfmtNative;
use serde_json::{Map, Value};

use crate::language::Language;
use crate::path_ignore::PathIgnore;

/// The formatter family an engine/language pair belongs to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    Js,
    Json,
    Yaml,
    Toml,
    Markdown,
    Css,
}

/// The languages oxfmt formats (and therefore owns `ignorePatterns` for).
pub(crate) fn is_oxfmt_language(language: &Language) -> bool {
    matches!(
        language,
        Language::JavaScript
            | Language::TypeScript
            | Language::Jsx
            | Language::Tsx
            | Language::Json
            | Language::Jsonc
            | Language::Yaml
            | Language::Toml
            | Language::Markdown
            | Language::Mdx
            | Language::Css
            | Language::Scss
            | Language::Less
            | Language::Html
            | Language::Vue
            | Language::GraphQl
    )
}

fn family(language: &Language, engine: &str) -> Option<Family> {
    match (engine, language) {
        ("oxc", Language::JavaScript | Language::TypeScript | Language::Jsx | Language::Tsx) => Some(Family::Js),
        ("oxc", Language::Json | Language::Jsonc) => Some(Family::Json),
        ("yaml", Language::Yaml) => Some(Family::Yaml),
        ("taplo", Language::Toml) => Some(Family::Toml),
        ("rumdl", Language::Markdown) => Some(Family::Markdown),
        ("malva", Language::Css | Language::Scss | Language::Less) => Some(Family::Css),
        _ => None,
    }
}

/// `[fmt.<lang>.<engine>]` defaults derived from the native config for one language/engine.
pub(crate) fn engine_defaults(native: &OxfmtNative, language: &Language, engine: &str) -> toml::Table {
    let Some(family) = family(language, engine) else {
        return toml::Table::new();
    };
    if !native.is_present() {
        return toml::Table::new();
    }
    let effective = effective_options(native, language);
    translate(family, &effective)
}

/// Report a native config that was found but could not be read, once.
pub(crate) fn warn_if_unusable(native: &OxfmtNative) {
    if let Some(error) = &native.error {
        warn_once(format!("oxfmt-unusable:{error}"), || {
            tracing::warn!("ignoring the oxfmt configuration: {error}");
        });
    }
}

fn effective_options(native: &OxfmtNative, language: &Language) -> Map<String, Value> {
    let mut merged = native.options.clone();
    for entry in &native.overrides {
        match override_scope(&entry.files) {
            Scope::Languages(languages) if languages.contains(language) => {
                if !entry.exclude_files.is_empty() {
                    warn_once(format!("oxfmt-exclude-files:{:?}", entry.files), || {
                        tracing::warn!(
                            "oxfmt override for {:?} uses `excludeFiles`, which poly cannot apply per path; \
                             the override is applied to every matching file",
                            entry.files
                        );
                    });
                }
                deep_merge(&mut merged, &entry.options);
            }
            Scope::Languages(_) => {}
            Scope::Partial => warn_once(format!("oxfmt-partial-override:{:?}", entry.files), || {
                tracing::warn!(
                    "oxfmt override for {:?} is scoped to part of a language and is not applied: poly resolves \
                     formatter options per language, not per path (options: {})",
                    entry.files,
                    entry.options.keys().cloned().collect::<Vec<_>>().join(", ")
                );
            }),
        }
    }
    merged
}

fn deep_merge(base: &mut Map<String, Value>, over: &Map<String, Value>) {
    for (key, value) in over {
        match (base.get_mut(key), value) {
            (Some(Value::Object(existing)), Value::Object(incoming)) => deep_merge(existing, incoming),
            _ => {
                base.insert(key.clone(), value.clone());
            }
        }
    }
}

enum Scope {
    Languages(Vec<Language>),
    Partial,
}

/// The languages an override's globs cover when every glob is a plain extension glob
/// (`**/*.json`, `*.{yaml,yml}`); anything path-scoped is [`Scope::Partial`].
fn override_scope(globs: &[String]) -> Scope {
    let mut languages = Vec::new();
    for glob in globs {
        let Some(extensions) = extension_glob(glob) else {
            return Scope::Partial;
        };
        for extension in extensions {
            if let Some(language) = Language::from_path(Path::new(&format!("x.{extension}"))) {
                languages.push(language);
            }
        }
    }
    if languages.is_empty() {
        Scope::Partial
    } else {
        Scope::Languages(languages)
    }
}

fn extension_glob(glob: &str) -> Option<Vec<String>> {
    let rest = glob.strip_prefix("**/").unwrap_or(glob).strip_prefix("*.")?;
    let extensions: Vec<String> = match rest.strip_prefix('{').and_then(|r| r.strip_suffix('}')) {
        Some(list) => list.split(',').map(str::to_owned).collect(),
        None => vec![rest.to_owned()],
    };
    extensions
        .iter()
        .all(|e| !e.is_empty() && e.chars().all(|c| c.is_ascii_alphanumeric()))
        .then_some(extensions)
}

fn translate(family: Family, options: &Map<String, Value>) -> toml::Table {
    let mut out = toml::Table::new();
    let mut put = |key: &str, value: toml::Value| {
        out.insert(key.to_owned(), value);
    };
    let text = |key: &str| options.get(key).and_then(Value::as_str);
    let flag = |key: &str| options.get(key).and_then(Value::as_bool);
    let number = |key: &str| options.get(key).and_then(Value::as_i64);

    put("line_width", toml::Value::Integer(number("printWidth").unwrap_or(100)));
    if let Some(width) = number("tabWidth") {
        put("indent_width", toml::Value::Integer(width));
    }
    if let Some(tabs) = flag("useTabs") {
        put(
            "indent_style",
            toml::Value::String(if tabs { "tab" } else { "space" }.into()),
        );
    }
    if let Some(ending) = text("endOfLine") {
        put("line_ending", toml::Value::String(ending.to_owned()));
    }
    let comma = text("trailingComma");
    match family {
        Family::Js => {
            if let Some(semi) = flag("semi") {
                put(
                    "semicolons",
                    toml::Value::String(if semi { "always" } else { "as-needed" }.into()),
                );
            }
            quote_style(&mut put, "quote_style", flag("singleQuote"));
            quote_style(&mut put, "jsx_quote_style", flag("jsxSingleQuote"));
            if let Some(props) = text("quoteProps") {
                put("quote_properties", toml::Value::String(props.to_owned()));
            }
            if let Some(comma) = comma {
                put("trailing_commas", toml::Value::String(comma.to_owned()));
            }
            if let Some(parens) = text("arrowParens") {
                let value = if parens == "avoid" { "as-needed" } else { parens };
                put("arrow_parentheses", toml::Value::String(value.to_owned()));
            }
            for (from, to) in [
                ("bracketSpacing", "bracket_spacing"),
                ("bracketSameLine", "bracket_same_line"),
            ] {
                if let Some(value) = flag(from) {
                    put(to, toml::Value::Boolean(value));
                }
            }
            if let Some(wrap) = text("objectWrap") {
                put("object_wrap", toml::Value::String(wrap.to_owned()));
            }
            if let Some(position) = text("experimentalOperatorPosition") {
                put("operator_position", toml::Value::String(position.to_owned()));
            }
            if let Some(single) = flag("singleAttributePerLine") {
                put("single_attribute_per_line", toml::Value::Boolean(single));
            }
            if let Some(sort) = options.get("sortImports").and_then(snake_case_keys) {
                put("sort_imports", sort);
            }
        }
        Family::Json => {
            if let Some(comma) = comma {
                put(
                    "trailing_commas",
                    toml::Value::String(if comma == "none" { "never" } else { "always" }.into()),
                );
            }
            if let Some(spacing) = flag("bracketSpacing") {
                put("bracket_spacing", toml::Value::Boolean(spacing));
            }
            if let Some(wrap) = text("objectWrap") {
                put("object_wrap", toml::Value::String(wrap.to_owned()));
            }
            if let Some(single) = flag("singleQuote") {
                put("single_quote", toml::Value::Boolean(single));
            }
            if let Some(props) = text("quoteProps") {
                put("quote_properties", toml::Value::String(props.to_owned()));
            }
            if let Some(sort) = options.get("sortPackageJson").and_then(snake_case_keys) {
                put("sort_package_json", sort);
            }
        }
        Family::Yaml => {
            if let Some(single) = flag("singleQuote") {
                put("single_quote", toml::Value::Boolean(single));
            }
            if let Some(spacing) = flag("bracketSpacing") {
                put("bracket_spacing", toml::Value::Boolean(spacing));
            }
            if let Some(comma) = comma {
                put(
                    "trailing_commas",
                    toml::Value::String(if comma == "none" { "never" } else { "always" }.into()),
                );
            }
            if let Some(wrap) = text("proseWrap") {
                put("prose_wrap", toml::Value::String(wrap.to_owned()));
            }
        }
        Family::Css => {
            if let Some(single) = flag("singleQuote") {
                put("single_quote", toml::Value::Boolean(single));
            }
            if let Some(comma) = comma {
                put(
                    "trailing_commas",
                    toml::Value::String(if comma == "none" { "never" } else { "always" }.into()),
                );
            }
        }
        Family::Markdown => {
            if let Some(single) = flag("singleQuote") {
                put("single_quote", toml::Value::Boolean(single));
            }
            if let Some(wrap) = text("proseWrap") {
                put("prose_wrap", toml::Value::String(wrap.to_owned()));
            }
        }
        Family::Toml => {
            if let Some(comma) = comma {
                put(
                    "trailing_commas",
                    toml::Value::String(if comma == "none" { "never" } else { "always" }.into()),
                );
            }
        }
    }
    report_unsupported(family, options);
    out
}

fn quote_style(put: &mut impl FnMut(&str, toml::Value), key: &str, single: Option<bool>) {
    if let Some(single) = single {
        put(
            key,
            toml::Value::String(if single { "single" } else { "double" }.into()),
        );
    }
}

fn report_unsupported(family: Family, options: &Map<String, Value>) {
    let enabled = |key: &str| match options.get(key) {
        None | Some(Value::Bool(false) | Value::Null) => false,
        Some(_) => true,
    };
    if family == Family::Js && enabled("sortTailwindcss") {
        warn_once("oxfmt-sort-tailwindcss".to_owned(), || {
            tracing::warn!(
                "oxfmt `sortTailwindcss` is not applied: its class sorter runs in JavaScript and cannot run \
                 in poly's in-process formatter, so Tailwind classes are left in source order"
            );
        });
    }
    for key in ["jsdoc", "experimentalTernaries", "embeddedLanguageFormatting"] {
        if family == Family::Js && enabled(key) {
            warn_once(format!("oxfmt-unsupported:{key}"), || {
                tracing::warn!("oxfmt option `{key}` is not supported by poly's oxc formatter and is ignored");
            });
        }
    }
    if options.get("insertFinalNewline") == Some(&Value::Bool(false)) {
        warn_once("oxfmt-final-newline".to_owned(), || {
            tracing::warn!(
                "oxfmt `insertFinalNewline = false` is not supported; poly follows `[defaults] final_newline`"
            );
        });
    }
}

fn snake_case_keys(value: &Value) -> Option<toml::Value> {
    Some(match value {
        Value::Null => return None,
        Value::Bool(flag) => toml::Value::Boolean(*flag),
        Value::Number(number) => toml::Value::Integer(number.as_i64()?),
        Value::String(text) => toml::Value::String(text.clone()),
        Value::Array(items) => toml::Value::Array(items.iter().filter_map(snake_case_keys).collect()),
        Value::Object(map) => toml::Value::Table(
            map.iter()
                .filter_map(|(key, value)| Some((camel_to_snake(key), snake_case_keys(value)?)))
                .collect(),
        ),
    })
}

fn camel_to_snake(key: &str) -> String {
    let mut out = String::with_capacity(key.len() + 2);
    for c in key.chars() {
        if c.is_ascii_uppercase() {
            out.push('_');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// `ignorePatterns` of a native oxfmt config, compiled once per plan.
#[derive(Debug)]
pub(crate) struct FormatIgnore(PathIgnore);

impl FormatIgnore {
    /// The ignore set for `language`, or `None` when oxfmt does not own it or ignores nothing.
    pub(crate) fn for_language(native: &OxfmtNative, language: &Language) -> Option<FormatIgnore> {
        if !native.is_present() || !is_oxfmt_language(language) {
            return None;
        }
        let root = native.dir()?.to_path_buf();
        PathIgnore::new(&[(root, native.ignore_patterns.clone())]).map(FormatIgnore)
    }

    /// Whether the oxfmt config says `path` is not to be formatted.
    pub(crate) fn is_ignored(&self, path: &Path) -> bool {
        self.0.is_ignored(path)
    }
}

fn warn_once(key: String, report: impl FnOnce()) {
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let first = SEEN
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .expect("warning set poisoned")
        .insert(key);
    if first {
        report();
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn native(json: &str) -> OxfmtNative {
        let value: Value = serde_json::from_str(json).unwrap();
        let Value::Object(mut options) = value else { panic!() };
        let ignore_patterns = options
            .remove("ignorePatterns")
            .map(|v| serde_json::from_value(v).unwrap())
            .unwrap_or_default();
        let overrides = options
            .remove("overrides")
            .map(|v| {
                v.as_array()
                    .unwrap()
                    .iter()
                    .map(|entry| poly_config::OxfmtOverride {
                        files: serde_json::from_value(entry["files"].clone()).unwrap(),
                        exclude_files: Vec::new(),
                        options: entry["options"].as_object().unwrap().clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        OxfmtNative {
            file: Some(PathBuf::from("/repo/.oxfmtrc.jsonc")),
            options,
            overrides,
            ignore_patterns,
            error: None,
        }
    }

    #[test]
    fn javascript_keys_follow_the_config() {
        let rc = native(r#"{"printWidth":120,"tabWidth":4,"singleQuote":true,"trailingComma":"es5","semi":false}"#);
        let table = engine_defaults(&rc, &Language::TypeScript, "oxc");
        assert_eq!(table["line_width"].as_integer(), Some(120));
        assert_eq!(table["indent_width"].as_integer(), Some(4));
        assert_eq!(table["quote_style"].as_str(), Some("single"));
        assert_eq!(table["trailing_commas"].as_str(), Some("es5"));
        assert_eq!(table["semicolons"].as_str(), Some("as-needed"));
    }

    #[test]
    fn extension_overrides_apply_to_their_language_only() {
        let rc = native(
            r#"{"tabWidth":4,"overrides":[{"files":["**/*.json","**/*.yaml","**/*.yml"],"options":{"tabWidth":2}}]}"#,
        );
        let tab =
            |language: &Language, engine: &str| engine_defaults(&rc, language, engine)["indent_width"].as_integer();
        assert_eq!(tab(&Language::TypeScript, "oxc"), Some(4));
        assert_eq!(tab(&Language::Json, "oxc"), Some(2));
        assert_eq!(tab(&Language::Yaml, "yaml"), Some(2));
    }

    #[test]
    fn path_scoped_overrides_are_not_applied() {
        let rc = native(r#"{"tabWidth":4,"overrides":[{"files":["products/**"],"options":{"tabWidth":8}}]}"#);
        assert_eq!(
            engine_defaults(&rc, &Language::TypeScript, "oxc")["indent_width"].as_integer(),
            Some(4)
        );
    }

    #[test]
    fn json_trailing_commas_use_the_json_vocabulary() {
        let rc = native(r#"{"trailingComma":"none"}"#);
        assert_eq!(
            engine_defaults(&rc, &Language::Json, "oxc")["trailing_commas"].as_str(),
            Some("never")
        );
    }

    #[test]
    fn sort_imports_keys_become_snake_case() {
        let rc = native(
            r#"{"sortImports":{"newlinesBetween":true,"internalPattern":["@/"],"groups":["builtin",["parent","sibling"]]}}"#,
        );
        let table = engine_defaults(&rc, &Language::Tsx, "oxc");
        let sort = table["sort_imports"].as_table().unwrap();
        assert_eq!(sort["newlines_between"].as_bool(), Some(true));
        assert_eq!(sort["internal_pattern"].as_array().unwrap().len(), 1);
        assert_eq!(sort["groups"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn sort_package_json_reaches_the_json_engine_only() {
        let rc = native(r#"{"sortPackageJson":{"sortScripts":true}}"#);
        let table = engine_defaults(&rc, &Language::Json, "oxc");
        assert_eq!(table["sort_package_json"]["sort_scripts"].as_bool(), Some(true));
        assert!(!engine_defaults(&rc, &Language::TypeScript, "oxc").contains_key("sort_package_json"));
    }

    #[test]
    fn unrelated_engines_get_nothing() {
        let rc = native(r#"{"tabWidth":4}"#);
        assert!(engine_defaults(&rc, &Language::Python, "ruff").is_empty());
        assert!(engine_defaults(&OxfmtNative::default(), &Language::Json, "oxc").is_empty());
    }

    #[test]
    fn ignore_patterns_cover_oxfmt_languages_only() {
        let rc = native(r#"{"ignorePatterns":["**/*.md","fixtures/**"]}"#);
        let ignore = FormatIgnore::for_language(&rc, &Language::Markdown).unwrap();
        assert!(ignore.is_ignored(Path::new("/repo/docs/a.md")));
        assert!(ignore.is_ignored(Path::new("/repo/fixtures/a/b.md")));
        assert!(!ignore.is_ignored(Path::new("/repo/docs/a.txt")));
        assert!(!ignore.is_ignored(Path::new("/elsewhere/a.md")));
        assert!(FormatIgnore::for_language(&rc, &Language::Python).is_none());
    }
}
