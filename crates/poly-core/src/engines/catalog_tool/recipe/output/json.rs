//! The JSON reports: actionlint, buf, hadolint, golangci-lint, buildifier,
//! tflint, npm-groovy-lint and gh-actions-updater (`gau --lint-pins`).
//!
//! Each reader pulls only the fields it needs through `serde_json::Value`, so a
//! tool adding a field never breaks it; a field it needs going missing, or the
//! document being something other than JSON, is an `Err`.

use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::{Finding, position};
use crate::engine::Severity;

/// The first JSON value in `text`. Some tools follow their report with a
/// plain-text summary (golangci-lint prints `7 issues:` after the document).
fn document(text: &str, tool: &str) -> Result<Value> {
    serde_json::Deserializer::from_str(text.trim())
        .into_iter::<Value>()
        .next()
        .with_context(|| format!("{tool} printed no JSON report"))?
        .with_context(|| format!("{tool} did not print a JSON report"))
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn number(value: &Value, key: &str) -> Option<u32> {
    position(value.get(key).and_then(Value::as_u64))
}

pub(super) fn actionlint(stdout: &str) -> Result<Vec<Finding>> {
    if stdout.trim().is_empty() {
        return Ok(Vec::new());
    }
    let report = document(stdout, "actionlint")?;
    let Some(items) = report.as_array() else {
        bail!("actionlint printed a JSON value that is not a list of findings");
    };
    Ok(items
        .iter()
        .map(|item| {
            let message = text(item, "message").unwrap_or_default();
            let kind = text(item, "kind");
            let severity = actionlint_severity(kind.as_deref(), &message);
            let mut finding = Finding::new(severity, message);
            finding.path = text(item, "filepath");
            finding.line = number(item, "line");
            finding.column = number(item, "column");
            finding.end_column = number(item, "end_column");
            finding.code = kind;
            finding
        })
        .collect())
}

/// actionlint has no severities — every finding fails it — so poly assigns one
/// per `kind`.
///
/// - `runner-label` is a warning: whether a label exists depends on the runners
///   the repository has registered, which a file cannot show. Self-hosted and
///   ARC labels are flagged until declared in `.github/actionlint.yaml`
///   (`self-hosted-runner.labels`), and before 0.30 this was a warning too.
/// - `shellcheck` takes the level shellcheck printed in the message
///   (`SC2086:info:…`), mapped as poly's own shellcheck backend maps it, so an
///   `info` or `style` note in a `run:` block does not fail the run.
/// - Every other kind (`syntax-check`, `expression`, `action`, `job-needs`, …)
///   is a defect in the workflow itself and stays an error.
fn actionlint_severity(kind: Option<&str>, message: &str) -> Severity {
    match kind {
        Some("runner-label") => Severity::Warning,
        Some("shellcheck") => shellcheck_level(message),
        _ => Severity::Error,
    }
}

/// The level in actionlint's `shellcheck reported issue in this script:
/// SC2086:info:1:6: …` message; `Warning` when the message has no level.
fn shellcheck_level(message: &str) -> Severity {
    let level = message
        .split_whitespace()
        .find(|word| {
            word.strip_prefix("SC")
                .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
        })
        .and_then(|code| code.split(':').nth(1))
        .unwrap_or_default();
    match level {
        "error" => Severity::Error,
        "info" => Severity::Info,
        "style" => Severity::Hint,
        _ => Severity::Warning,
    }
}

/// buf prints one JSON object per line.
pub(super) fn buf(stdout: &str) -> Result<Vec<Finding>> {
    stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let item = document(line, "buf")?;
            let mut finding = Finding::new(Severity::Error, text(&item, "message").unwrap_or_default());
            finding.path = text(&item, "path");
            finding.line = number(&item, "start_line");
            finding.column = number(&item, "start_column");
            finding.end_line = number(&item, "end_line");
            finding.end_column = number(&item, "end_column");
            finding.code = text(&item, "type");
            Ok(finding)
        })
        .collect()
}

pub(super) fn hadolint(stdout: &str) -> Result<Vec<Finding>> {
    if stdout.trim().is_empty() {
        return Ok(Vec::new());
    }
    let report = document(stdout, "hadolint")?;
    let Some(items) = report.as_array() else {
        bail!("hadolint printed a JSON value that is not a list of findings");
    };
    Ok(items
        .iter()
        .map(|item| {
            let severity = match text(item, "level").as_deref() {
                Some("error") => Severity::Error,
                Some("warning") => Severity::Warning,
                Some("info") => Severity::Info,
                _ => Severity::Hint,
            };
            let mut finding = Finding::new(severity, text(item, "message").unwrap_or_default());
            finding.path = text(item, "file");
            finding.line = number(item, "line");
            finding.column = number(item, "column");
            finding.code = text(item, "code");
            finding
        })
        .collect())
}

pub(super) fn golangci(stdout: &str) -> Result<Vec<Finding>> {
    let report = document(stdout, "golangci-lint")?;
    let issues = report
        .get("Issues")
        .context("golangci-lint's JSON report has no Issues field")?;
    let Some(items) = issues.as_array() else {
        return Ok(Vec::new());
    };
    Ok(items
        .iter()
        .map(|item| {
            let severity = match text(item, "Severity").as_deref() {
                Some("warning") => Severity::Warning,
                Some("info") => Severity::Info,
                _ => Severity::Error,
            };
            let mut finding = Finding::new(severity, text(item, "Text").unwrap_or_default());
            finding.code = text(item, "FromLinter");
            if let Some(pos) = item.get("Pos") {
                finding.path = text(pos, "Filename");
                finding.line = number(pos, "Line");
                finding.column = number(pos, "Column");
            }
            finding.fixable = item
                .get("SuggestedFixes")
                .and_then(Value::as_array)
                .is_some_and(|fixes| !fixes.is_empty())
                || item
                    .get("Replacement")
                    .is_some_and(|replacement| !replacement.is_null());
            finding
        })
        .collect())
}

/// buildifier's `-format=json`. A file with a syntax error is reported as
/// `valid: false` with the message on stderr (`path:line:col: message`).
pub(super) fn buildifier(stdout: &str, stderr: &str) -> Result<Vec<Finding>> {
    let report = document(stdout, "buildifier")?;
    let files = report
        .get("files")
        .and_then(Value::as_array)
        .context("buildifier's JSON report has no files list")?;
    let mut findings = Vec::new();
    for file in files {
        let path = text(file, "filename");
        if file.get("valid").and_then(Value::as_bool) == Some(false) {
            findings.push(syntax_error(path.as_deref(), stderr));
        }
        for warning in file.get("warnings").and_then(Value::as_array).into_iter().flatten() {
            let mut finding = Finding::new(Severity::Error, text(warning, "message").unwrap_or_default());
            finding.path = path.clone();
            finding.code = text(warning, "category");
            finding.url = text(warning, "url");
            finding.fixable = warning.get("autoFixable").and_then(Value::as_bool).unwrap_or(false);
            if let Some(start) = warning.get("start") {
                finding.line = number(start, "line");
                finding.column = number(start, "column");
            }
            if let Some(end) = warning.get("end") {
                finding.end_line = number(end, "line");
                finding.end_column = number(end, "column");
            }
            findings.push(finding);
        }
    }
    Ok(findings)
}

/// The `path:line:col: message` line buildifier wrote for `path`, or a generic
/// message when stderr does not carry one.
fn syntax_error(path: Option<&str>, stderr: &str) -> Finding {
    let mut finding = Finding::new(Severity::Error, "syntax error");
    finding.path = path.map(str::to_owned);
    finding.code = Some("syntax".to_owned());
    let located = path.and_then(|path| {
        stderr
            .lines()
            .find_map(|line| line.strip_prefix(path)?.strip_prefix(':'))
    });
    if let Some(rest) = located {
        let mut parts = rest.splitn(3, ':');
        finding.line = parts.next().and_then(|n| n.trim().parse().ok());
        finding.column = parts.next().and_then(|n| n.trim().parse().ok());
        if let Some(message) = parts.next() {
            finding.message = message.trim().to_owned();
        }
    }
    finding
}

pub(super) fn tflint(stdout: &str) -> Result<Vec<Finding>> {
    let report = document(stdout, "tflint")?;
    let mut findings = Vec::new();
    for issue in report.get("issues").and_then(Value::as_array).into_iter().flatten() {
        let rule = issue.get("rule");
        let severity = match rule.and_then(|rule| text(rule, "severity")).as_deref() {
            Some("warning") => Severity::Warning,
            Some("notice") => Severity::Info,
            _ => Severity::Error,
        };
        let mut finding = Finding::new(severity, text(issue, "message").unwrap_or_default());
        finding.code = rule.and_then(|rule| text(rule, "name"));
        finding.url = rule.and_then(|rule| text(rule, "link"));
        apply_range(&mut finding, issue.get("range"));
        findings.push(finding);
    }
    for error in report.get("errors").and_then(Value::as_array).into_iter().flatten() {
        let message = text(error, "message")
            .or_else(|| text(error, "summary"))
            .unwrap_or_else(|| "tflint failed".to_owned());
        let mut finding = Finding::new(Severity::Error, message);
        finding.code = Some("tflint-error".to_owned());
        apply_range(&mut finding, error.get("range"));
        findings.push(finding);
    }
    Ok(findings)
}

fn apply_range(finding: &mut Finding, range: Option<&Value>) {
    let Some(range) = range else { return };
    finding.path = text(range, "filename");
    if let Some(start) = range.get("start") {
        finding.line = number(start, "line");
        finding.column = number(start, "column");
    }
    if let Some(end) = range.get("end") {
        finding.end_line = number(end, "line");
        finding.end_column = number(end, "column");
    }
}

/// npm-groovy-lint's `--output json`: `files` maps an absolute path to its
/// `errors`, each with a 1-based line and a 0-based `range` column.
pub(super) fn groovy(stdout: &str) -> Result<Vec<Finding>> {
    let start = stdout
        .find('{')
        .context("npm-groovy-lint did not print a JSON report")?;
    let report = document(&stdout[start..], "npm-groovy-lint")?;
    let files = report
        .get("files")
        .and_then(Value::as_object)
        .context("npm-groovy-lint's JSON report has no files map")?;
    let mut findings = Vec::new();
    for (path, file) in files {
        for error in file.get("errors").and_then(Value::as_array).into_iter().flatten() {
            let severity = match text(error, "severity").as_deref() {
                Some("error") => Severity::Error,
                Some("warning") => Severity::Warning,
                _ => Severity::Info,
            };
            let mut finding = Finding::new(severity, text(error, "msg").unwrap_or_default());
            finding.path = Some(path.clone());
            finding.line = number(error, "line");
            finding.code = text(error, "rule");
            if let Some(range) = error.get("range") {
                if let Some(start) = range.get("start") {
                    finding.column = start.get("character").and_then(Value::as_u64).map(|c| c as u32 + 1);
                }
                if let Some(end) = range.get("end") {
                    finding.end_line = number(end, "line");
                    finding.end_column = end.get("character").and_then(Value::as_u64).map(|c| c as u32 + 1);
                }
            }
            findings.push(finding);
        }
    }
    Ok(findings)
}

/// gh-actions-updater's `--lint-pins --format json`: a `summary` with counts,
/// one `findings` entry per remote `uses:` ref that is not a bare `vN` major
/// tag (exit 1), and `diagnostics` for files it could not parse (exit 3).
///
/// The summary's finding count must match the list, so a report cut short is
/// an error rather than fewer findings.
pub(super) fn gau(stdout: &str) -> Result<Vec<Finding>> {
    let report = document(stdout, "gau")?;
    let expected = report
        .get("summary")
        .and_then(|summary| summary.get("findings"))
        .and_then(Value::as_u64)
        .context("gau's JSON report has no summary.findings count")?;
    let items = report
        .get("findings")
        .and_then(Value::as_array)
        .context("gau's JSON report has no findings list")?;
    if usize::try_from(expected).ok() != Some(items.len()) {
        bail!(
            "gau's summary counts {expected} finding(s) but its report lists {}",
            items.len()
        );
    }
    let mut findings: Vec<Finding> = items
        .iter()
        .map(|item| {
            let uses = text(item, "uses").unwrap_or_default();
            let reason = text(item, "reason").unwrap_or_default();
            let mut finding = Finding::new(Severity::Error, format!("{uses}: {reason}"));
            finding.path = text(item, "file");
            finding.line = number(item, "line");
            finding.code = text(item, "kind");
            finding
        })
        .collect();
    for diagnostic in report
        .get("diagnostics")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let mut finding = Finding::new(Severity::Error, text(diagnostic, "message").unwrap_or_default());
        finding.path = text(diagnostic, "file");
        finding.line = number(diagnostic, "line");
        finding.code = text(diagnostic, "code");
        findings.push(finding);
    }
    Ok(findings)
}
