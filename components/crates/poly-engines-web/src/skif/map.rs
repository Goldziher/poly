//! skif's report as poly diagnostics, with the safe fixes it plans as byte edits.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use poly_engine_api::engine::{Diagnostic, Edit, Severity, Span};
use skif::{FileChange, FileSystem, FixSafety, FixSelection, Report, Workspace};

/// Maps `report` to findings grouped by the file they are in; `host_path` turns a path relative to the
/// workspace root into the path the host knows the file by.
pub(super) fn findings(
    report: &Report,
    workspace: &Workspace,
    fs: &dyn FileSystem,
    host_path: &dyn Fn(&Path) -> PathBuf,
) -> Vec<(PathBuf, Vec<Diagnostic>)> {
    let mut grouped: BTreeMap<PathBuf, Vec<Diagnostic>> = BTreeMap::new();
    for finding in &report.diagnostics {
        let fix = safe_fix(finding, workspace, fs);
        grouped
            .entry(host_path(&finding.location.path))
            .or_default()
            .push(diagnostic(finding, fix));
    }
    grouped.into_iter().collect()
}

fn diagnostic(finding: &skif::Diagnostic, fix: Vec<Edit>) -> Diagnostic {
    let mut metadata = BTreeMap::new();
    for (key, value) in [
        ("package", &finding.package),
        ("dependency", &finding.dependency),
        ("specifier", &finding.specifier),
    ] {
        if let Some(value) = value {
            metadata.insert(key.to_owned(), value.clone());
        }
    }
    Diagnostic {
        engine: super::ENGINE_NAME.to_owned(),
        code: Some(finding.rule.as_str().to_owned()),
        severity: match finding.severity {
            skif::Severity::Error => Severity::Error,
            skif::Severity::Warning => Severity::Warning,
            _ => Severity::Info,
        },
        title: finding.message.clone(),
        description: finding.help.clone(),
        span: finding.location.span.map(|span| Span {
            start_line: span.start_line,
            start_col: span.start_col,
            end_line: span.end_line,
            end_col: span.end_col,
        }),
        url: None,
        fix,
        metadata,
    }
}

/// The edits of `finding`'s safe fix, when it has one that touches only the file the finding is in.
///
/// A poly fix edits the finding's own file; a skif fix that spans files (a catalog entry in
/// `pnpm-workspace.yaml` for a dependency in a manifest) cannot be expressed that way and stays a
/// suggestion. Unsafe fixes are never planned: the selection's ceiling is [`FixSafety::Safe`].
fn safe_fix(finding: &skif::Diagnostic, workspace: &Workspace, fs: &dyn FileSystem) -> Vec<Edit> {
    if finding.fixes.is_empty() {
        return Vec::new();
    }
    let alone = Report::new(vec![finding.clone()], 1);
    let Ok(plan) = skif::apply_fixes(workspace, &alone, &FixSelection::new(FixSafety::Safe), fs) else {
        return Vec::new();
    };
    match plan.changes.as_slice() {
        [FileChange::Modify { path, edits, .. }] if *path == finding.location.path => edits
            .iter()
            .map(|edit| Edit {
                start_byte: edit.start as usize,
                end_byte: edit.end as usize,
                replacement: edit.replacement.clone(),
            })
            .collect(),
        _ => Vec::new(),
    }
}
