//! adrlint backend — MADR architecture decision record lint.
//!
//! Wraps [`adrlint`](https://crates.io/crates/adrlint) in-process — no subprocess, no system
//! dependency. adrlint is the MADR linter: it parses a decision record once with
//! `pulldown-cmark` and judges its section structure, links, metadata and list style.
//!
//! # What this backend runs
//!
//! Per file, the **document** rules — `MADR01`–`MADR08`, `MADR12b`/`MADR12d`, `MADR21` and
//! `MADR31` — over one numbered ADR (`NNNN-*.md`), a pure function of that file.
//!
//! As a **project check** ([`project`]), the tree-wide rules over the whole ADR log: numbering,
//! gaps, duplicates and file names (`MADR41a`, `MADR41b`, `MADR42`, `MADR43`, `MADR44`) and the
//! immutability lock (`ADR50`/`ADR51`). The host hands over the log's Markdown files and
//! `adrlint.lock` as content and caches the result on every one of them, so a sibling ADR or the
//! lock changing re-runs the check and nothing else does.
//!
//! Two groups of adrlint rules stay in the `adrlint` CLI:
//!
//! - **Filesystem link checks** (`MADR12a`/`MADR12c`) read the rest of the tree, which neither
//!   tier may do: a target appearing or disappearing would not invalidate a cached "missing link"
//!   finding. The remaining `MADR12` variants — anchor resolution and an absolute-path rejection —
//!   are pure and do run.
//! - **External links** (`MADR11`) need the network.
//!
//! Markdown quality is not duplicated either: the `rumdl` backend already lints Markdown, so this
//! backend does not call adrlint's embedded rumdl layer.
//!
//! # Opt-in
//!
//! **Off by default.** A repository's existing ADR log may follow a different template, and
//! enabling MADR enforcement unasked would turn a green run red on the upgrade. Enable it with
//! `[lint.markdown.adrlint] enabled = true`.
//!
//! # Configuration
//!
//! Rule selection follows poly's uniform vocabulary ([ADR 0016]): `select` replaces the active
//! set, `extend_select` adds to it, and `ignore` removes from it. Codes are matched
//! case-insensitively by prefix, as adrlint's own `[rules]` table does, so `MADR07` covers
//! `MADR07a` and `MADR07b`. `[lint.markdown.adrlint.rules.<id>] level` moves an individual
//! finding's severity through the runner's normal remap.
//!
//! [ADR 0016]: ../../../../adrs/0016-uniform-rule-selection.md

mod project;

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ::adrlint::adr;
use ::adrlint::diagnostics::{self as adr_diagnostics, Diagnostic as AdrDiagnostic, Severity as AdrSeverity};

use poly_engine_api::config::EngineConfig;
use poly_engine_api::engine::{
    Capabilities, Diagnostic, Edit, Engine, OptionKeys, OptionTable, OptionType, ProjectDirs, ProjectInputs,
    ProjectRequest, ProjectSideFile, Severity, SourceFile, Span,
};
use poly_engine_api::language::Language;
use poly_engine_api::rule_config::string_list;

/// Cache-key identity: the wrapped crate version plus a marker for this backend's own mapping.
///
/// Bump the suffix whenever the diagnostic/edit mapping below changes, and the version whenever
/// `adrlint` is upgraded — either alters output, and a stale cache entry would survive both.
const VERSION: &str = "adrlint-0.5.0+poly3";

/// Why a Markdown file is routed past this backend.
const NOT_AN_ADR: &str = "not a numbered ADR file";

/// `MADR12` variants whose answer depends on the filesystem rather than the document.
///
/// `MADR12a` (a relative destination exists) and `MADR12c` (a root-relative one does) read the
/// tree around the ADR. poly's result cache keys on the ADR's own bytes, so a target file created
/// later would not invalidate a cached "missing" finding; the `adrlint` CLI checks these against
/// the live tree instead. `MADR12b` (an anchor matches a heading) and `MADR12d` (an absolute path
/// cannot be rendered) are pure and stay.
const FILESYSTEM_LINK_CODES: &[&str] = &["MADR12a", "MADR12c"];

static LANGUAGES: &[Language] = &[Language::Markdown];

/// MADR architecture decision record linter. See the module docs.
pub struct AdrlintEngine;

impl Engine for AdrlintEngine {
    fn overridden_hooks(&self) -> u32 {
        poly_engine_api::desc::hooks::PROVIDES_LANGUAGE | poly_engine_api::desc::hooks::SKIP_REASON
    }

    fn name(&self) -> &'static str {
        "adrlint"
    }

    fn languages(&self) -> &'static [Language] {
        LANGUAGES
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            lint: true,
            // adrlint's formatter *is* rumdl, which poly already runs.
            format: false,
            fix: true,
            project: true,
        }
    }

    /// `[lint.markdown.adrlint]` reads the universal `enabled` plus the `select` /
    /// `extend_select` / `ignore` subset of the uniform vocabulary. `rules` is not declared
    /// here because it is universal for a per-language table (see
    /// [`poly_engine_api::engine::UNIVERSAL_OPTION_KEYS`]) and the runner applies `level` itself.
    fn option_keys(&self, table: OptionTable) -> OptionKeys {
        match table {
            OptionTable::Lint => OptionKeys::declared(&[
                ("enabled", OptionType::BOOLEAN),
                ("select", OptionType::ARRAY),
                ("extend_select", OptionType::ARRAY),
                ("ignore", OptionType::ARRAY),
                ("dir", OptionType::STRING),
                ("paths", OptionType::ARRAY),
                ("recursive", OptionType::BOOLEAN),
                ("start", OptionType::INTEGER),
                ("allow_gaps", OptionType::BOOLEAN),
                ("lock", OptionType::STRING),
            ]),
            OptionTable::Format => OptionKeys::declared(&[]),
            OptionTable::CrossCuttingLint => OptionKeys::UNCHECKED,
        }
    }

    fn version(&self) -> &str {
        VERSION
    }

    /// Off unless switched on, and then only for a numbered ADR.
    ///
    /// Claiming Markdown lint coverage while switched off would let a repository that also
    /// disabled `rumdl` read as covered when nothing inspected its Markdown.
    fn provides_language_lint(&self, language: &Language, cfg: &EngineConfig) -> bool {
        *language == Language::Markdown && enabled(cfg)
    }

    fn skip_reason(&self, src: &SourceFile) -> Option<Cow<'static, str>> {
        (!is_adr_file(&src.path)).then_some(Cow::Borrowed(NOT_AN_ADR))
    }

    /// The tree-wide rules are switched on by `enabled`, need `adrlint.lock`, and are about one
    /// ADR directory: the host hands over only the Markdown under it, plus a name-only listing of
    /// the other files there, which is all `MADR42` needs.
    fn project_inputs(&self) -> ProjectInputs {
        ProjectInputs {
            gate_key: Some("enabled".to_owned()),
            files: vec![ProjectSideFile {
                option_key: "lock".to_owned(),
                default_path: project::DEFAULT_LOCK.to_owned(),
            }],
            dirs: Some(ProjectDirs {
                dir_key: "dir".to_owned(),
                dir_default: project::DEFAULT_DIR.to_owned(),
                paths_key: Some("paths".to_owned()),
                recursive_key: Some("recursive".to_owned()),
                recursive_default: false,
                whole_tree: false,
                file_names: Vec::new(),
                list_unserved: true,
            }),
        }
    }

    fn project_check(&self, req: &ProjectRequest<'_>) -> anyhow::Result<Vec<(PathBuf, Vec<Diagnostic>)>> {
        project::check(req)
    }

    fn lint(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        if !enabled(cfg) || !is_adr_file(&src.path) {
            return Ok(Vec::new());
        }

        let document = adr::parse(&src.content);
        let root = project_root(&src.path);
        let mut findings = ::adrlint::rules::check_document(&document, &src.path, &root);
        adr_diagnostics::locate(&mut findings, &src.content);
        ::adrlint::fix::mark_fixable(&mut findings, &document, &src.content);
        adr_diagnostics::add_default_help(&mut findings);

        let selection = Selection::from_options(cfg);
        let fix_offsets = heading_fix_offsets(&src.content, &document);
        Ok(findings
            .into_iter()
            .filter(|finding| !FILESYSTEM_LINK_CODES.contains(&finding.code))
            .filter(|finding| selection.keeps(finding.code))
            .map(|finding| map_diagnostic(finding, &fix_offsets))
            .collect())
    }
}

/// Whether `path` is a Markdown file adrlint should judge: a numbered ADR, `NNNN-*.md`.
///
/// The gate is the file name alone — the same rule adrlint's own discovery applies — so a README
/// or template inside a log is never treated as a decision record.
fn is_adr_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(::adrlint::discovery::is_numbered)
}

/// The project root the link rules resolve root-relative destinations against.
///
/// poly hands a backend a file, not a project, so the root is the nearest ancestor holding a
/// `.git` directory or a `poly.toml`/`poly.local.toml`, falling back to the ADR's own directory.
fn project_root(path: &Path) -> PathBuf {
    path.ancestors()
        .find(|directory| {
            directory.join(".git").exists()
                || directory.join(poly_config::CONFIG_FILE_NAMES[0]).is_file()
                || directory.join(poly_config::LOCAL_OVERRIDE_NAME).is_file()
        })
        .or_else(|| path.parent())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// Byte offsets at which a `#` turns a misplaced `##` heading into `###`.
///
/// The structural fix is the only autofix adrlint applies, and it is located by line number.
/// Offset 0 of the source is always a valid edit position, so the map is keyed by line.
fn heading_fix_offsets(source: &str, document: &adr::Document) -> BTreeMap<usize, usize> {
    let lines = ::adrlint::fix::misplaced_heading_lines(document, source);
    if lines.is_empty() {
        return BTreeMap::new();
    }
    let mut offsets = BTreeMap::new();
    let mut cursor = 0;
    for (index, line) in source.split_inclusive('\n').enumerate() {
        if lines.contains(&(index + 1)) {
            let indent = line.len() - line.trim_start().len();
            offsets.insert(index + 1, cursor + indent);
        }
        cursor += line.len();
    }
    offsets
}

/// Maps one adrlint finding onto poly's normalised diagnostic.
fn map_diagnostic(finding: AdrDiagnostic, fix_offsets: &BTreeMap<usize, usize>) -> Diagnostic {
    let start_line = u32::try_from(finding.line).unwrap_or(u32::MAX);
    let start_col = u32::try_from(finding.column).unwrap_or(u32::MAX);
    let span = Span {
        start_line,
        start_col,
        end_line: u32::try_from(finding.end_line.unwrap_or(finding.line)).unwrap_or(u32::MAX),
        end_col: u32::try_from(finding.end_column.unwrap_or(finding.column)).unwrap_or(u32::MAX),
    };
    let fix = fix_offsets
        .get(&finding.line)
        .filter(|_| finding.fixable)
        .map(|offset| {
            vec![Edit {
                start_byte: *offset,
                end_byte: *offset,
                replacement: "#".to_owned(),
            }]
        })
        .unwrap_or_default();
    Diagnostic {
        engine: "adrlint".to_owned(),
        code: Some(finding.code.to_owned()),
        severity: match finding.severity {
            AdrSeverity::Error => Severity::Error,
            AdrSeverity::Warning => Severity::Warning,
        },
        title: finding.message,
        description: finding.help,
        span: Some(span),
        url: None,
        fix,
        metadata: BTreeMap::new(),
    }
}

/// The `select` / `extend_select` / `ignore` subset of the uniform rule vocabulary.
struct Selection {
    select: Vec<String>,
    extend_select: Vec<String>,
    ignore: Vec<String>,
}

impl Selection {
    fn from_options(cfg: &EngineConfig) -> Self {
        // `rules` as an array is the uniform vocabulary's second spelling of an
        // allow-list; the table form carries per-rule parameters the runner applies.
        let select = string_list(cfg, "select");
        let rules = string_list(cfg, "rules");
        Self {
            select: if select.is_empty() { rules } else { select },
            extend_select: string_list(cfg, "extend_select"),
            ignore: string_list(cfg, "ignore"),
        }
    }

    /// Whether a finding survives the configured selection.
    ///
    /// An empty `select` means every rule is active; `extend_select` still contributes when
    /// `select` narrowed the set. A code matches a pattern when the pattern is a case-insensitive
    /// prefix, so `MADR07` covers both `MADR07a` and `MADR07b`.
    ///
    /// `MADR31` (list marker `*`) is off unless a `select` / `extend_select` pattern names it: poly's
    /// Markdown formatter writes `-` and has no marker option, so a default-on `MADR31` would flag
    /// the formatter's own output with no fix that survives `fmt`.
    fn keeps(&self, code: &str) -> bool {
        let matches = |patterns: &[String]| patterns.iter().any(|pattern| matches_code(pattern, code));
        if !self.select.is_empty() && !matches(&self.select) && !matches(&self.extend_select) {
            return false;
        }
        if code == LIST_MARKER_CODE && !self.names_list_marker() {
            return false;
        }
        !matches(&self.ignore)
    }

    fn names_list_marker(&self) -> bool {
        self.select
            .iter()
            .chain(&self.extend_select)
            .any(|pattern| pattern.trim() != "*" && matches_code(pattern, LIST_MARKER_CODE))
    }
}

/// The adrlint rule that wants `*` list markers, which the Markdown formatter cannot produce.
const LIST_MARKER_CODE: &str = "MADR31";

fn matches_code(pattern: &str, code: &str) -> bool {
    let pattern = pattern.trim();
    !pattern.is_empty()
        && (pattern == "*"
            || code
                .get(..pattern.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(pattern)))
}

/// The universal `enabled` key, defaulting to `false`: this backend is opt-in.
fn enabled(cfg: &EngineConfig) -> bool {
    cfg.options
        .get("enabled")
        .and_then(toml::Value::as_bool)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(options: &[(&str, toml::Value)]) -> EngineConfig {
        EngineConfig {
            globals: poly_engine_api::config::GlobalDefaults::default(),
            indent_width: 2,
            options: options.iter().cloned().map(|(k, v)| (k.to_owned(), v)).collect(),
        }
    }

    fn source(path: &str, content: &str) -> SourceFile {
        SourceFile {
            path: PathBuf::from(path),
            language: Language::Markdown,
            content: content.into(),
        }
    }

    const MADR: &str = "\
# Use Rust

## Context and Problem Statement

Text.

## Considered Options

* Rust

## Decision Outcome

Chosen option: Rust, because it is fast.
";

    #[test]
    fn should_do_nothing_when_disabled() {
        let findings = AdrlintEngine
            .lint(&source("adrs/0001-use-rust.md", "# Broken\n"), &config(&[]))
            .expect("lint");
        assert!(findings.is_empty());
    }

    #[test]
    fn should_report_a_missing_madr_section_when_enabled() {
        let findings = AdrlintEngine
            .lint(
                &source("adrs/0001-use-rust.md", "# Use Rust\n"),
                &config(&[("enabled", toml::Value::Boolean(true))]),
            )
            .expect("lint");
        assert!(
            findings
                .iter()
                .any(|finding| finding.code.as_deref() == Some("MADR01b")),
            "got {:?}",
            findings.iter().map(|finding| &finding.code).collect::<Vec<_>>()
        );
    }

    #[test]
    fn should_accept_a_well_formed_madr_record() {
        let findings = AdrlintEngine
            .lint(
                &source("adrs/0001-use-rust.md", MADR),
                &config(&[("enabled", toml::Value::Boolean(true))]),
            )
            .expect("lint");
        assert!(
            findings.is_empty(),
            "got {:?}",
            findings.iter().map(|f| &f.code).collect::<Vec<_>>()
        );
    }

    #[test]
    fn should_ignore_markdown_that_is_not_a_numbered_adr() {
        let found = AdrlintEngine
            .lint(
                &source("README.md", "# Broken\n"),
                &config(&[("enabled", toml::Value::Boolean(true))]),
            )
            .expect("lint");
        assert!(found.is_empty());
        assert_eq!(
            AdrlintEngine.skip_reason(&source("README.md", "# Broken\n")).as_deref(),
            Some(NOT_AN_ADR)
        );
    }

    #[test]
    fn should_not_report_filesystem_link_checks() {
        let content = format!("{MADR}\nSee [the plan](missing.md) and [the root](/missing.md).\n");
        let findings = AdrlintEngine
            .lint(
                &source("adrs/0001-use-rust.md", &content),
                &config(&[("enabled", toml::Value::Boolean(true))]),
            )
            .expect("lint");
        assert!(
            !findings
                .iter()
                .any(|finding| matches!(finding.code.as_deref(), Some("MADR12a" | "MADR12c")))
        );
    }

    #[test]
    fn should_drop_ignored_codes() {
        let findings = AdrlintEngine
            .lint(
                &source("adrs/0001-use-rust.md", "# Use Rust\n"),
                &config(&[
                    ("enabled", toml::Value::Boolean(true)),
                    ("ignore", toml::Value::Array(vec!["MADR01".into()])),
                ]),
            )
            .expect("lint");
        assert!(
            findings
                .iter()
                .all(|finding| !finding.code.as_deref().is_some_and(|code| code.starts_with("MADR01")))
        );
    }

    #[test]
    fn should_only_keep_selected_codes() {
        let findings = AdrlintEngine
            .lint(
                &source("adrs/0001-use-rust.md", "# Use Rust\n\n- item\n"),
                &config(&[
                    ("enabled", toml::Value::Boolean(true)),
                    ("select", toml::Value::Array(vec!["MADR31".into()])),
                ]),
            )
            .expect("lint");
        assert!(!findings.is_empty());
        assert!(findings.iter().all(|finding| finding.code.as_deref() == Some("MADR31")));
    }

    #[test]
    fn should_not_flag_dash_bullets_unless_the_list_marker_rule_is_selected() {
        let content = format!("{MADR}\n- extra\n");
        let run = |extra: &[(&str, toml::Value)]| {
            let mut options = vec![("enabled", toml::Value::Boolean(true))];
            options.extend_from_slice(extra);
            AdrlintEngine
                .lint(&source("adrs/0001-use-rust.md", &content), &config(&options))
                .expect("lint")
        };
        assert!(run(&[]).iter().all(|f| f.code.as_deref() != Some("MADR31")));
        let extended = run(&[("extend_select", toml::Value::Array(vec!["MADR31".into()]))]);
        assert!(extended.iter().any(|f| f.code.as_deref() == Some("MADR31")));
    }

    #[test]
    fn should_ignore_blank_selection_patterns() {
        assert!(!matches_code("", "MADR01a"));
        assert!(!matches_code("  ", "MADR01a"));
        assert!(matches_code("MADR01", "MADR01a"));
    }

    #[test]
    fn should_attach_a_heading_fix_only_to_the_misplaced_section() {
        let content = "\
# Use Rust

## Context and Problem Statement

Text.

## Decision Outcome

Chosen option: Rust, because it is fast.

## Consequences

Good.
";
        let findings = AdrlintEngine
            .lint(
                &source("adrs/0001-use-rust.md", content),
                &config(&[("enabled", toml::Value::Boolean(true))]),
            )
            .expect("lint");
        let fixable = findings.iter().find(|finding| !finding.fix.is_empty());
        let finding = fixable.expect("the Consequences heading is fixable");
        assert_eq!(finding.fix.len(), 1);
        assert_eq!(finding.fix[0].replacement, "#");
    }
}
