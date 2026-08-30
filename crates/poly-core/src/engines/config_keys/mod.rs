//! The dead-config check for `poly.toml` itself.
//!
//! `[lint.*]` and `[fmt.*]` are raw `toml::Table`s: no schema, no
//! `deny_unknown_fields`, so **every** key a user writes parses by
//! construction. A misspelled or unsupported key is therefore
//! indistinguishable from one that works — which is the root cause ADR 0016's
//! 2026-08-29 amendment (§4) records behind an entire family of dead config
//! keys that shipped, documented as working, for several releases.
//!
//! This backend closes that gap. Each engine declares what it reads, and the
//! type it reads it as ([`Engine::option_keys`]); this backend lints the
//! `poly.toml` file itself and reports two things:
//!
//! - `unknown-config-key` — a key in an engine's table that the engine's
//!   declaration does not cover at all.
//! - `invalid-config-value` — a key the engine *does* read, given a value of a
//!   type its accessor cannot use. `mccabe_max_complexity = "oops"` is read with
//!   `as_integer`, which answers `None`, and ruff silently keeps its default
//!   (issue #16). Two codes rather than one, so `[per-file-ignores]` and rule
//!   selection can name either independently.
//!
//! ## Why a backend, and not a run-level message
//!
//! A `poly.toml` is a file in the tree like any other, so linting it needs no
//! new channel: the findings travel the normal diagnostic path and therefore
//! reach `pretty`, `json` and `toon` identically, carry a real source span, are
//! suppressible and cacheable like any other diagnostic, and — since a config
//! file is one file — are reported **once per run**, never once per linted file.
//! In a monorepo each nested `poly.toml` (ADR 0018) is checked where it sits,
//! against the keys written in *it*, which is what a reader can act on.
//!
//! ## What is deliberately not reported
//!
//! - **Sub-table contents of `rules`.** `[rules.<id>]` holds `level` plus
//!   arbitrary tool parameters by design (ADR 0016), so its keys are open-ended.
//! - **Unknown language ids.** [`Language::Other`] means any tree-sitter pack id
//!   is a legitimate language, so `[fmt.<id>.<tool>]` cannot be judged.
//! - **Unknown tool names.** A `[tools.<name>]` catalog tool (ADR 0013) takes a
//!   config table named after itself and may be declared in a base config this
//!   file cannot see, so an unrecognised tool name is left alone.
//! - **The value type of a key only a serde probe recognises.** The wide
//!   upstream formatter tables (malva, markup_fmt, pretty_yaml, pretty_graphql,
//!   mago) derive their key set from the type they deserialize into, which
//!   names no expected type poly could quote back. A key with no declared type
//!   is checked for existence only.
//!
//! Each of those is a false-positive risk, and a warning nobody can trust is
//! worse than the silence it replaces.

use std::ops::Range;
use std::path::Path;

use crate::config::{EngineConfig, Kind};
use crate::engine::{
    Capabilities, Diagnostic, Engine, OptionKeys, OptionTable, OptionType, Severity, SourceFile, Span,
};
use crate::language::Language;
use crate::registry::{all_languages, engines_for};

pub(crate) mod probe;
#[cfg(test)]
mod tests;

pub(crate) use probe::{recognized_by_deserialize, recognized_by_type_probe};

/// Config file names this backend checks. Mirrors `poly_config`'s own
/// `CONFIG_FILE_NAMES` + `LOCAL_OVERRIDE_NAME`; a file poly does not load as
/// config must not be reported against a config schema.
const CONFIG_FILE_NAMES: &[&str] = &[poly_config::CONFIG_FILE_NAMES[0], poly_config::LOCAL_OVERRIDE_NAME];

/// Languages this backend serves. TOML only — `poly.toml` is a TOML file.
const LANGUAGES: &[Language] = &[Language::Toml];

/// Rule code of a key no engine reads, so `[per-file-ignores]` and `--ignore`
/// can name it and it reads consistently in JSON.
const UNKNOWN_KEY_CODE: &str = "unknown-config-key";

/// Rule code of a key that *is* read but was given an unusable value. Distinct
/// from [`UNKNOWN_KEY_CODE`] so the two can be selected and ignored separately.
const INVALID_VALUE_CODE: &str = "invalid-config-value";

/// Bumped whenever the reported set changes: it is folded into the cache key,
/// and a stale entry would replay warnings computed under an older schema.
/// `2`: `invalid-config-value` joined `unknown-config-key`.
const VERSION: &str = "3";

/// The `poly.toml` schema backend: reports keys under `[lint.*]` / `[fmt.*]`
/// that no engine reads, and keys whose value no engine can use.
#[derive(Debug, Default, Clone, Copy)]
pub struct PolyConfigEngine;

impl Engine for PolyConfigEngine {
    fn name(&self) -> &'static str {
        "polyconfig"
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
        VERSION
    }

    /// `false`: this backend checks poly's own config file, it does not lint
    /// TOML. Claiming TOML lint coverage here would let a repo whose only TOML
    /// finding is a config typo report itself as "TOML linted".
    fn provides_language_lint(&self, _language: &Language, _cfg: &EngineConfig) -> bool {
        false
    }

    fn option_keys(&self, _table: OptionTable) -> OptionKeys {
        // The check itself takes no configuration; a key in
        // `[lint.toml.polyconfig]` reads nothing.
        OptionKeys::declared(&[])
    }

    fn lint(&self, src: &SourceFile, _cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        if !is_config_file(&src.path) {
            return Ok(Vec::new());
        }
        Ok(config_key_diagnostics(&src.content, self.name()))
    }
}

/// Whether `path` is a config file poly actually loads.
fn is_config_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| CONFIG_FILE_NAMES.contains(&name))
}

/// One finding about a key, resolved to the table it sits in.
struct Finding {
    /// Dotted path of the table, e.g. `lint.python.ruff`.
    table: String,
    /// The offending key.
    key: String,
    /// Engine the table belongs to.
    engine: &'static str,
    /// What is wrong with it.
    problem: Problem,
}

/// The three ways a key can be dead config.
enum Problem {
    /// A top-level section name the schema does not have. Distinct from
    /// [`Problem::UnknownKey`] because nothing is scoped to an engine here — the
    /// whole section is discarded, not one setting inside it.
    UnknownTopLevel,
    /// No engine reads the key.
    UnknownKey {
        /// Keys that table does accept, for the hint.
        known: Vec<&'static str>,
        /// Extra guidance from the engine's declaration.
        note: Option<&'static str>,
    },
    /// The key is read, but not at the type it was given.
    InvalidValue {
        /// The type the backend's accessor reads the key as.
        expected: OptionType,
        /// The type the value actually has.
        found: String,
    },
}

/// Check the `[lint.*]` / `[fmt.*]` tables of one `poly.toml` source.
fn config_key_diagnostics(source: &str, engine_name: &str) -> Vec<Diagnostic> {
    let Ok(document) = source.parse::<toml::Table>() else {
        // Not our error to report: an unparsable `poly.toml` fails the run the
        // moment anything tries to load it.
        return Vec::new();
    };
    let spans = KeySpans::parse(source);
    let mut findings: Vec<Finding> = Vec::new();
    // Top-level sections first, so an unrecognised one is reported before any
    // finding from inside a table that *is* recognised.
    for name in document.keys() {
        if !poly_config::TOP_LEVEL_KEYS.contains(&name.as_str()) {
            findings.push(Finding {
                table: String::new(),
                key: name.clone(),
                engine: "poly",
                problem: Problem::UnknownTopLevel,
            });
        }
    }
    for (section, kind) in [("lint", Kind::Lint), ("fmt", Kind::Format)] {
        let Some(table) = document.get(section).and_then(toml::Value::as_table) else {
            continue;
        };
        collect_section(section, kind, table, &mut findings);
    }
    findings
        .into_iter()
        .map(|finding| diagnostic(finding, &spans, engine_name))
        .collect()
}

/// Walk one `[lint]` / `[fmt]` section: cross-cutting engine tables at the top
/// level, `<lang>.<engine>` tables below it.
fn collect_section(section: &str, kind: Kind, table: &toml::Table, findings: &mut Vec<Finding>) {
    let cross_cutting = cross_cutting_engines();
    for (name, value) in table {
        let Some(inner) = value.as_table() else {
            continue;
        };
        if let Some(engine) = cross_cutting.iter().find(|engine| engine.name() == name) {
            // `[lint.typos]` and friends: the language-agnostic table, whose
            // merge in `Config::engine_config` drops the universal keys.
            let table_kind = match kind {
                Kind::Lint => OptionTable::CrossCuttingLint,
                Kind::Format => OptionTable::Format,
            };
            collect_table(
                &format!("{section}.{name}"),
                engine.as_ref(),
                engine.option_keys(table_kind),
                inner,
                false,
                findings,
            );
            continue;
        }
        let language = language_from_id(name);
        let engines = engines_for(&language);
        for (tool, tool_value) in inner {
            let Some(tool_table) = tool_value.as_table() else {
                continue;
            };
            // An unrecognised tool name is left alone: it may be a catalog tool
            // (ADR 0013) declared in a base config this file cannot see.
            let Some(engine) = engines.iter().find(|engine| engine.name() == tool) else {
                continue;
            };
            let table_kind = match kind {
                Kind::Lint => OptionTable::Lint,
                Kind::Format => OptionTable::Format,
            };
            collect_table(
                &format!("{section}.{name}.{tool}"),
                engine.as_ref(),
                engine.option_keys(table_kind),
                tool_table,
                true,
                findings,
            );
        }
    }
}

/// Report every key of `table` the declaration does not accept, and every
/// accepted key whose value the backend cannot read.
fn collect_table(
    path: &str,
    engine: &dyn Engine,
    keys: OptionKeys,
    table: &toml::Table,
    universal: bool,
    findings: &mut Vec<Finding>,
) {
    if !keys.is_checked() {
        return;
    }
    for (key, value) in table {
        let problem = if keys.accepts(key, table, universal) {
            // A key with no declared type (one only a serde probe recognises)
            // reports nothing: there is no expected type to quote back.
            match keys.value_problem(key, value, universal) {
                Some(expected) => Problem::InvalidValue {
                    expected,
                    found: OptionType::describe_value(value),
                },
                None => continue,
            }
        } else {
            Problem::UnknownKey {
                known: keys.known_keys(universal),
                note: keys.note(),
            }
        };
        findings.push(Finding {
            table: path.to_string(),
            key: key.clone(),
            engine: engine.name(),
            problem,
        });
    }
}

/// Render one finding, anchored at the offending key when its span is known.
fn diagnostic(finding: Finding, spans: &KeySpans, engine_name: &str) -> Diagnostic {
    let Finding {
        table,
        key,
        engine,
        problem,
    } = finding;
    let (code, title, description) = match problem {
        Problem::UnknownTopLevel => unknown_top_level_text(&key),
        Problem::UnknownKey { known, note } => unknown_key_text(&table, &key, engine, &known, note),
        Problem::InvalidValue { expected, found } => invalid_value_text(&table, &key, engine, expected, &found),
    };
    Diagnostic {
        engine: engine_name.to_string(),
        code: Some(code.to_string()),
        severity: Severity::Warning,
        title,
        description: Some(description),
        span: spans.span(&table, &key),
        url: None,
        fix: Vec::new(),
        metadata: std::collections::BTreeMap::new(),
    }
}

/// Code, title and description for a top-level section poly does not have.
///
/// Worth its own wording: an unknown key inside `[lint.python.ruff]` costs the
/// reader one setting, while an unknown section costs them everything written
/// under it — a misspelled `[discovry]` silently drops every exclusion it
/// holds, and the only symptom is poly checking more files than expected, which
/// reads as poly being wrong rather than the config being wrong.
fn unknown_top_level_text(key: &str) -> (&'static str, String, String) {
    let title = format!("unknown top-level key `{key}`: poly.toml has no such section");
    let description = format!(
        "Everything under `{key}` is ignored. Top-level keys poly.toml reads: {}.",
        poly_config::TOP_LEVEL_KEYS.join(", ")
    );
    (UNKNOWN_KEY_CODE, title, description)
}

/// Code, title and description for a key no engine reads.
fn unknown_key_text(
    table: &str,
    key: &str,
    engine: &str,
    known: &[&'static str],
    note: Option<&'static str>,
) -> (&'static str, String, String) {
    let mut description = if known.is_empty() {
        format!("`[{table}]` reads no options at all, so `{key}` has no effect.")
    } else {
        format!("Keys `[{table}]` reads: {}.", known.join(", "))
    };
    if let Some(note) = note {
        description.push(' ');
        description.push_str(note);
    }
    let title = format!("unknown option `{key}` in `[{table}]`: the {engine} backend does not read it");
    (UNKNOWN_KEY_CODE, title, description)
}

/// Code, title and description for a key given a value its reader cannot use.
///
/// Names the key, both types, and the consequence — the reader has to know that
/// the setting is *not* in force, which is the whole difference from a typo.
fn invalid_value_text(
    table: &str,
    key: &str,
    engine: &str,
    expected: OptionType,
    found: &str,
) -> (&'static str, String, String) {
    let expected = expected.describe();
    let title = format!("invalid value for `{key}` in `[{table}]`: expected {expected}, found {found}");
    let description = format!(
        "The {engine} backend reads `{key}` as {expected}. {found_capitalized} cannot be applied, so the \
         value is discarded and `{key}` has no effect. Set it to {expected}, or remove the key.",
        found_capitalized = capitalize(found)
    );
    (INVALID_VALUE_CODE, title, description)
}

/// Uppercase the first character, so a noun phrase can open a sentence.
fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Resolve a config table's language id. Any id the tree-sitter pack knows is a
/// language ([`Language::Other`]), so an unmatched id is not an error.
fn language_from_id(id: &str) -> Language {
    all_languages()
        .into_iter()
        .find(|language| language.id() == id)
        .unwrap_or_else(|| Language::Other(id.to_string()))
}

/// The backends that take a language-agnostic `[lint.<engine>]` table: those the
/// registry appends for every language, which is exactly the set declaring no
/// languages of its own.
fn cross_cutting_engines() -> Vec<Box<dyn Engine>> {
    engines_for(&Language::Other(String::new()))
        .into_iter()
        .filter(|engine| engine.languages().is_empty())
        .collect()
}

/// Line/column lookup for the keys of a `poly.toml`, built once per file.
///
/// `toml::Table` drops positions, so the document is parsed a second time with
/// `toml_edit`, which keeps them. Without this a warning could name the key but
/// not point at it — and the whole point is to send the reader to the line.
struct KeySpans {
    document: Option<toml_edit::Document<String>>,
    /// Byte offset of the start of each line, for offset → line/column.
    line_starts: Vec<usize>,
}

impl KeySpans {
    fn parse(source: &str) -> KeySpans {
        let mut line_starts = vec![0];
        line_starts.extend(source.match_indices('\n').map(|(index, _)| index + 1));
        KeySpans {
            document: toml_edit::Document::parse(source.to_string()).ok(),
            line_starts,
        }
    }

    /// Span of `key` inside the table at dotted `path`, if it can be located.
    ///
    /// An empty `path` means the document root, where the top-level section
    /// names live. Splitting `""` yields one empty component, so the descent
    /// has to be skipped rather than run with it.
    fn span(&self, path: &str, key: &str) -> Option<Span> {
        let document = self.document.as_ref()?;
        let mut table: &dyn toml_edit::TableLike = document.as_table();
        if !path.is_empty() {
            for component in path.split('.') {
                table = table.get(component)?.as_table_like()?;
            }
        }
        let (key, _) = table.get_key_value(key)?;
        self.to_span(key.span()?)
    }

    /// Convert a byte range into a 1-based line/column span.
    fn to_span(&self, range: Range<usize>) -> Option<Span> {
        let (start_line, start_col) = self.position(range.start)?;
        let (end_line, end_col) = self.position(range.end)?;
        Some(Span {
            start_line,
            start_col,
            end_line,
            end_col,
        })
    }

    fn position(&self, offset: usize) -> Option<(u32, u32)> {
        let line = self.line_starts.partition_point(|start| *start <= offset).max(1) - 1;
        let column = offset.checked_sub(*self.line_starts.get(line)?)?;
        Some((u32::try_from(line).ok()? + 1, u32::try_from(column).ok()? + 1))
    }
}
