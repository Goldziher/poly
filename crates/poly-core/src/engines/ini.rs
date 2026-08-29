//! INI linter backend, wrapping `rust-ini` (crate `rust-ini`, lib name `ini`).
//!
//! # Lint-only — no formatter, ever
//!
//! `rust-ini`'s parser (`Parser::parse_comment`) consumes a comment to end of
//! line and **discards its text** — the parsed [`ini::Ini`] model carries no
//! comments at all. Writing that model back out (`Ini::write_to`) would
//! silently delete every comment in the file. This backend therefore declares
//! `format: false` and never calls `write_to`; formatting for INI stays with
//! poly's tree-sitter generic tier, which preserves comments structurally.
//! `fix: false` too: nothing here proposes edits.
//!
//! # Two data sources
//!
//! `rule-error` (a real syntax error) is checked using `rust-ini`'s own
//! parser (`Ini::load_from_str_opt`), so its line/column come straight from
//! `ParseError`. The other five rules — `duplicate-key`, `duplicate-section`,
//! `key-without-value`, `inconsistent-separator`, `trailing-whitespace` — are
//! computed by a lightweight line-by-line scan of the raw text instead of the
//! parsed `Ini` model: `Ini`'s section map silently merges (or, depending on
//! parse mode, replaces) a repeated `[section]` header and its `Properties`
//! carry no source line, so there is no way to recover the line/column a
//! `Diagnostic` needs from the parsed model alone. The scan is best-effort —
//! it does not implement `rust-ini`'s full quoting/escaping/line-continuation
//! grammar — which is an acceptable trade for a lint-only style checker.
//!
//! # Separator heuristic
//!
//! A property line's separator is `=` if the line contains one, else `:` if
//! it contains one, else "no separator" (a bare key). Preferring `=`
//! unconditionally (rather than "whichever character appears first") matters
//! for real `.npmrc` files: a line like
//! `//registry.npmjs.org/:_authToken=${NPM_TOKEN}` contains a `:` *before*
//! its real `=` separator (it is a scoped npm config key, not two INI
//! key/value pairs), and "first character wins" would misread `:` as the
//! separator and then flag every other `=`-separated line in the file as
//! `inconsistent-separator`.
//!
//! # File detection (default set)
//!
//! Extensions: `.ini`, `.cfg`, `.desktop`, `.pypirc`. Filenames: `.npmrc`,
//! `.editorconfig`, `.coveragerc`, `.pylintrc`, `pylintrc`, `.flake8`,
//! `.gitlint`, `.hgrc` (see [`crate::language`]). Deliberately excluded:
//! `*.conf` (mostly not INI), `*.properties` (Java, different syntax),
//! `.gitconfig` (quoted subsections `rust-ini` cannot parse), systemd units
//! (legal duplicate keys are normal there), `*.reg`. `[lint.ini] extra_files`
//! is not implemented — deferred, see the backend-author report.

use std::collections::{BTreeMap, HashMap, HashSet};

use ini::{Ini, ParseOption};

use super::rule_config::RuleSelection;
use crate::config::EngineConfig;
use crate::engine::{Capabilities, Diagnostic, Engine, Severity, SourceFile, Span};
use crate::language::Language;

/// Cache-key version: the `rust-ini` crate version, plus a marker for this
/// backend's own scan/mapping logic. Bump whenever the crate is updated OR
/// the scan logic changes.
const INI_VERSION: &str = "rust-ini-0.21.3+map1";

const PARSE_ERROR: &str = "parse-error";
const DUPLICATE_KEY: &str = "duplicate-key";
const DUPLICATE_SECTION: &str = "duplicate-section";
const KEY_WITHOUT_VALUE: &str = "key-without-value";
const INCONSISTENT_SEPARATOR: &str = "inconsistent-separator";
const TRAILING_WHITESPACE: &str = "trailing-whitespace";

static LANGUAGES: &[Language] = &[Language::Ini];

/// INI linter backend. See the module docs — lint-only, never autofixes or
/// reformats.
pub struct IniEngine;

impl Engine for IniEngine {
    fn name(&self) -> &'static str {
        "ini"
    }

    fn languages(&self) -> &'static [Language] {
        LANGUAGES
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            lint: true,
            format: false,
            fix: false,
        }
    }

    fn version(&self) -> &str {
        INI_VERSION
    }

    fn lint(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        let selection = RuleSelection::from_options(cfg);

        let mut diags = parse_error_diagnostics(&src.content);
        diags.extend(scan_diagnostics(&src.content));

        Ok(apply_rule_selection(diags, &selection))
    }
}

/// Run `rust-ini`'s real parser purely to surface a genuine syntax error with
/// its real line/column. The parsed `Ini` value itself is discarded — see the
/// module docs for why the other five rules use a separate line scan instead.
fn parse_error_diagnostics(content: &str) -> Vec<Diagnostic> {
    match Ini::load_from_str_opt(content, ParseOption::default()) {
        Ok(_) => Vec::new(),
        Err(error) => {
            let line = error.line as u32;
            let col = error.col.max(1) as u32;
            vec![make_diag(
                PARSE_ERROR,
                Severity::Error,
                format!("parse error: {}", error.msg),
                Span {
                    start_line: line,
                    start_col: col,
                    end_line: line,
                    end_col: col.saturating_add(1),
                },
            )]
        }
    }
}

/// The five style rules computed by a plain line-by-line scan (see module
/// docs). Runs once per file, single pass over `content.lines()`.
fn scan_diagnostics(content: &str) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let mut current_section: Option<String> = None;
    let mut seen_sections: HashSet<String> = HashSet::new();
    let mut seen_keys: HashMap<Option<String>, HashSet<String>> = HashMap::new();
    let mut established_separator: Option<char> = None;

    for (index, raw_line) in content.lines().enumerate() {
        let line_no = (index + 1) as u32;
        let trimmed_end = raw_line.trim_end();
        if trimmed_end.len() != raw_line.len() {
            diags.push(make_diag(
                TRAILING_WHITESPACE,
                Severity::Warning,
                "trailing whitespace",
                line_span(line_no, raw_line.len() as u32),
            ));
        }

        let line = trimmed_end.trim_start();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }

        if let Some(name) = section_header_name(line) {
            if !seen_sections.insert(name.clone()) {
                diags.push(make_diag(
                    DUPLICATE_SECTION,
                    Severity::Warning,
                    format!("section `[{name}]` is duplicated"),
                    line_span(line_no, line.len() as u32),
                ));
            }
            current_section = Some(name);
            continue;
        }

        match find_separator(line) {
            None => {
                diags.push(make_diag(
                    KEY_WITHOUT_VALUE,
                    Severity::Warning,
                    format!("key `{line}` has no value"),
                    line_span(line_no, line.len() as u32),
                ));
            }
            Some((sep, sep_index)) => {
                match established_separator {
                    None => established_separator = Some(sep),
                    Some(established) if established != sep => {
                        diags.push(make_diag(
                            INCONSISTENT_SEPARATOR,
                            Severity::Warning,
                            format!("uses `{sep}` as a separator; the file otherwise uses `{established}`"),
                            line_span(line_no, line.len() as u32),
                        ));
                    }
                    _ => {}
                }

                let key = line[..sep_index].trim();
                let value = line[sep_index + 1..].trim();
                if value.is_empty() {
                    diags.push(make_diag(
                        KEY_WITHOUT_VALUE,
                        Severity::Warning,
                        format!("key `{key}` has no value"),
                        line_span(line_no, line.len() as u32),
                    ));
                }

                let keys_in_section = seen_keys.entry(current_section.clone()).or_default();
                if !keys_in_section.insert(key.to_owned()) {
                    diags.push(make_diag(
                        DUPLICATE_KEY,
                        Severity::Warning,
                        format!("key `{key}` is duplicated"),
                        line_span(line_no, line.len() as u32),
                    ));
                }
            }
        }
    }

    diags
}

/// Extract a `[section]` header's name from an already-trimmed, non-empty
/// line, or `None` if it is not a section header. Tolerates a missing
/// closing `]` (treats the rest of the line as the name) so a malformed
/// header is still recognized as *a section header attempt* by this scan —
/// `rust-ini`'s real parser separately reports the missing `]` as a genuine
/// `parse-error`.
fn section_header_name(line: &str) -> Option<String> {
    let rest = line.strip_prefix('[')?;
    let name = rest.find(']').map_or(rest, |end| &rest[..end]);
    Some(name.trim().to_owned())
}

/// Find the property separator in a line: `=` if present, else `:` if
/// present, else `None` (a bare key with no separator at all). See the module
/// docs for why `=` always wins over `:` rather than "whichever comes first".
fn find_separator(line: &str) -> Option<(char, usize)> {
    if let Some(index) = line.find('=') {
        return Some(('=', index));
    }
    line.find(':').map(|index| (':', index))
}

fn line_span(line: u32, len: u32) -> Span {
    Span {
        start_line: line,
        start_col: 1,
        end_line: line,
        end_col: len.saturating_add(1),
    }
}

fn make_diag(code: &str, severity: Severity, title: impl Into<String>, span: Span) -> Diagnostic {
    Diagnostic {
        engine: "ini".to_owned(),
        code: Some(code.to_owned()),
        severity,
        title: title.into(),
        description: None,
        span: Some(span),
        url: None,
        fix: Vec::new(),
        metadata: BTreeMap::new(),
    }
}

/// Filter/relevel diagnostics per `[lint.ini] select` / `extend_select` /
/// `ignore` / `[rules.<code>]`, mirroring `dockerfile.rs`'s
/// `apply_rule_selection`.
fn apply_rule_selection(diags: Vec<Diagnostic>, selection: &RuleSelection) -> Vec<Diagnostic> {
    if selection.is_empty() {
        return diags;
    }
    let keep: Vec<&String> = selection.select.iter().chain(selection.extend_select.iter()).collect();
    let matches = |code: &str, patterns: &[&String]| patterns.iter().any(|p| code == p.as_str());
    let ignore: Vec<&String> = selection.ignore.iter().collect();

    diags
        .into_iter()
        .filter(|d| {
            let code = d.code.as_deref().unwrap_or_default();
            let selected = keep.is_empty() || matches(code, &keep);
            selected && !matches(code, &ignore)
        })
        .map(|mut d| {
            if let Some(options) = selection.rules.get(d.code.as_deref().unwrap_or_default())
                && let Some(level) = options.level
            {
                d.severity = level;
            }
            d
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::config::GlobalDefaults;

    use super::*;

    fn engine_cfg() -> EngineConfig {
        EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 4,
            options: toml::Table::new(),
        }
    }

    fn make_src(content: &str) -> SourceFile {
        SourceFile {
            path: "test.ini".into(),
            language: Language::Ini,
            content: content.into(),
        }
    }

    #[test]
    fn clean_file_has_no_diagnostics() {
        let engine = IniEngine;
        let src = make_src("[server]\nhost=localhost\nport=8080\n");
        let diags = engine.lint(&src, &engine_cfg()).unwrap();
        assert!(diags.is_empty(), "expected no diagnostics for a clean file: {diags:?}");
    }

    #[test]
    fn npmrc_shaped_flat_file_has_no_false_positives() {
        let engine = IniEngine;
        let src = make_src(
            "registry=https://registry.npmjs.org/\n\
             save-exact=true\n\
             //registry.npmjs.org/:_authToken=${NPM_TOKEN}\n",
        );
        let diags = engine.lint(&src, &engine_cfg()).unwrap();
        assert!(
            diags.is_empty(),
            "a flat .npmrc-shaped file must not false-positive: {diags:?}"
        );
    }

    #[test]
    fn duplicate_key_within_a_section_is_flagged() {
        let engine = IniEngine;
        let src = make_src("[a]\nfoo=1\nfoo=2\n");
        let diags = engine.lint(&src, &engine_cfg()).unwrap();
        assert!(diags.iter().any(|d| d.code.as_deref() == Some(DUPLICATE_KEY)));
    }

    #[test]
    fn same_key_in_different_sections_is_not_flagged() {
        let engine = IniEngine;
        let src = make_src("[a]\nfoo=1\n[b]\nfoo=2\n");
        let diags = engine.lint(&src, &engine_cfg()).unwrap();
        assert!(!diags.iter().any(|d| d.code.as_deref() == Some(DUPLICATE_KEY)));
    }

    #[test]
    fn ignore_suppresses_selected_rule() {
        let engine = IniEngine;
        let src = make_src("[a]\nfoo=1\nfoo=2\n");
        let cfg = EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 4,
            options: toml::from_str(r#"ignore = ["duplicate-key"]"#).unwrap(),
        };
        let diags = engine.lint(&src, &cfg).unwrap();
        assert!(!diags.iter().any(|d| d.code.as_deref() == Some(DUPLICATE_KEY)));
    }
}
