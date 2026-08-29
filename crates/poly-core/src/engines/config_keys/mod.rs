//! The unknown-option-key check for `poly.toml` itself.
//!
//! `[lint.*]` and `[fmt.*]` are raw `toml::Table`s: no schema, no
//! `deny_unknown_fields`, so **every** key a user writes parses by
//! construction. A misspelled or unsupported key is therefore
//! indistinguishable from one that works — which is the root cause ADR 0016's
//! 2026-08-29 amendment (§4) records behind an entire family of dead config
//! keys that shipped, documented as working, for several releases.
//!
//! This backend closes that gap. Each engine declares what it reads
//! ([`Engine::option_keys`]); this backend lints the `poly.toml` file itself and
//! reports every key in an engine's table that the engine's declaration does not
//! cover.
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
//!
//! Each of those is a false-positive risk, and a warning nobody can trust is
//! worse than the silence it replaces.

use std::ops::Range;
use std::path::Path;

use crate::config::{EngineConfig, Kind};
use crate::engine::{Capabilities, Diagnostic, Engine, OptionKeys, OptionTable, Severity, SourceFile, Span};
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

/// Rule code of every finding, so `[per-file-ignores]` and `--ignore` can name
/// it and it reads consistently in JSON.
const UNKNOWN_KEY_CODE: &str = "unknown-config-key";

/// Bumped whenever the reported set changes: it is folded into the cache key,
/// and a stale entry would replay warnings computed under an older schema.
const VERSION: &str = "1";

/// The `poly.toml` schema backend: reports keys under `[lint.*]` / `[fmt.*]`
/// that no engine reads.
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
        Ok(unknown_key_diagnostics(&src.content, self.name()))
    }
}

/// Whether `path` is a config file poly actually loads.
fn is_config_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| CONFIG_FILE_NAMES.contains(&name))
}

/// One unknown key, resolved to the table it sits in.
struct UnknownKey {
    /// Dotted path of the table, e.g. `lint.python.ruff`.
    table: String,
    /// The offending key.
    key: String,
    /// Engine the table belongs to.
    engine: &'static str,
    /// Keys that table does accept, for the hint.
    known: Vec<&'static str>,
    /// Extra guidance from the engine's declaration.
    note: Option<&'static str>,
}

/// Check the `[lint.*]` / `[fmt.*]` tables of one `poly.toml` source.
fn unknown_key_diagnostics(source: &str, engine_name: &str) -> Vec<Diagnostic> {
    let Ok(document) = source.parse::<toml::Table>() else {
        // Not our error to report: an unparsable `poly.toml` fails the run the
        // moment anything tries to load it.
        return Vec::new();
    };
    let spans = KeySpans::parse(source);
    let mut findings = Vec::new();
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
fn collect_section(section: &str, kind: Kind, table: &toml::Table, findings: &mut Vec<UnknownKey>) {
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

/// Report every key of `table` the declaration does not accept.
fn collect_table(
    path: &str,
    engine: &dyn Engine,
    keys: OptionKeys,
    table: &toml::Table,
    universal: bool,
    findings: &mut Vec<UnknownKey>,
) {
    if !keys.is_checked() {
        return;
    }
    for key in table.keys() {
        if keys.accepts(key, table, universal) {
            continue;
        }
        findings.push(UnknownKey {
            table: path.to_string(),
            key: key.clone(),
            engine: engine.name(),
            known: keys.known_keys(universal),
            note: keys.note(),
        });
    }
}

/// Render one finding, anchored at the offending key when its span is known.
fn diagnostic(finding: UnknownKey, spans: &KeySpans, engine_name: &str) -> Diagnostic {
    let UnknownKey {
        table,
        key,
        engine,
        known,
        note,
    } = finding;
    let mut description = if known.is_empty() {
        format!("`[{table}]` reads no options at all, so `{key}` has no effect.")
    } else {
        format!("Keys `[{table}]` reads: {}.", known.join(", "))
    };
    if let Some(note) = note {
        description.push(' ');
        description.push_str(note);
    }
    Diagnostic {
        engine: engine_name.to_string(),
        code: Some(UNKNOWN_KEY_CODE.to_string()),
        severity: Severity::Warning,
        title: format!("unknown option `{key}` in `[{table}]`: the {engine} backend does not read it"),
        description: Some(description),
        span: spans.span(&table, &key),
        url: None,
        fix: Vec::new(),
        metadata: std::collections::BTreeMap::new(),
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
    fn span(&self, path: &str, key: &str) -> Option<Span> {
        let document = self.document.as_ref()?;
        let mut table: &dyn toml_edit::TableLike = document.as_table();
        for component in path.split('.') {
            table = table.get(component)?.as_table_like()?;
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
