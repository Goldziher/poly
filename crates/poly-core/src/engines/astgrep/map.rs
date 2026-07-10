//! Map ast-grep scan results to poly [`Diagnostic`]s.
//!
//! Two categories arrive from `CombinedScan::scan`:
//! - `diffs` — fixable matches: emit a `Diagnostic` with a non-empty `fix` vec
//!   built from the `NodeMatch`'s byte range and the rule's `Fixer`.
//! - `matches` — lint-only matches: emit a `Diagnostic` with an empty `fix` vec.

use ast_grep_config::{RuleConfig, Severity as AsgSeverity};
use ast_grep_core::NodeMatch;
use ast_grep_core::tree_sitter::StrDoc;

use crate::engine::{Diagnostic, Edit, Severity, Span};

use super::language::TslpLanguage;

/// Convert a fixable ast-grep diff (rule + matched node) to a [`Diagnostic`]
/// carrying the byte-range autofix edits.
pub fn diff_to_diagnostic(
    engine_name: &str,
    rule: &RuleConfig<TslpLanguage>,
    node_match: &NodeMatch<'_, StrDoc<TslpLanguage>>,
) -> Diagnostic {
    let fixes = fix_edits(rule, node_match);
    build_diagnostic(engine_name, rule, node_match, fixes)
}

/// Build the byte-range autofix [`Edit`]s for a matched node from the rule's
/// `Fixer`. Empty when the rule declares no `fix`. Shared by
/// [`diff_to_diagnostic`] and the rule-test runner so the CLI-visible fix and
/// the tested fix come from one code path.
pub fn fix_edits(rule: &RuleConfig<TslpLanguage>, node_match: &NodeMatch<'_, StrDoc<TslpLanguage>>) -> Vec<Edit> {
    rule.fixer
        .iter()
        .map(|fixer| {
            let edit = node_match.make_edit(&rule.matcher, fixer);
            Edit {
                start_byte: edit.position,
                end_byte: edit.position + edit.deleted_length,
                replacement: String::from_utf8_lossy(&edit.inserted_text).into_owned(),
            }
        })
        .collect()
}

/// Convert a lint-only ast-grep match (rule + matched nodes) to a
/// [`Diagnostic`] with no fix edits.
pub fn match_to_diagnostic(
    engine_name: &str,
    rule: &RuleConfig<TslpLanguage>,
    node_match: &NodeMatch<'_, StrDoc<TslpLanguage>>,
) -> Diagnostic {
    build_diagnostic(engine_name, rule, node_match, Vec::new())
}

fn build_diagnostic(
    engine_name: &str,
    rule: &RuleConfig<TslpLanguage>,
    node_match: &NodeMatch<'_, StrDoc<TslpLanguage>>,
    fix: Vec<Edit>,
) -> Diagnostic {
    let span = {
        let start = node_match.start_pos();
        let end = node_match.end_pos();
        Span {
            start_line: (start.line() + 1) as u32,
            start_col: (start.column(node_match) + 1) as u32,
            end_line: (end.line() + 1) as u32,
            end_col: (end.column(node_match) + 1) as u32,
        }
    };

    let message = rule.get_message(node_match);

    Diagnostic {
        engine: engine_name.to_string(),
        code: Some(rule.id.clone()),
        severity: map_severity(&rule.severity),
        title: message,
        description: rule.note.as_deref().map(str::to_string),
        span: Some(span),
        url: rule.url.as_deref().map(str::to_string),
        fix,
        metadata: std::collections::BTreeMap::new(),
    }
}

/// Map ast-grep `Severity` to poly `Severity`.
fn map_severity(s: &AsgSeverity) -> Severity {
    match s {
        AsgSeverity::Error => Severity::Error,
        AsgSeverity::Warning => Severity::Warning,
        AsgSeverity::Info => Severity::Info,
        AsgSeverity::Hint | AsgSeverity::Off => Severity::Hint,
    }
}
