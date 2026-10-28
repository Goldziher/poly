//! `helm lint` / `helm template` text and generic `path:line:col: message`
//! reports.
//!
//! helm prints `[LEVEL] location: message` lines for lint and `Error: ...` for
//! a failed render, with the template position embedded in the message
//! (`execution error at (chart/templates/secret.yaml:6:11): ...`). There is no
//! machine format, so the shape is read from the lines.

use std::sync::OnceLock;

use regex::Regex;

use super::Finding;
use crate::engine::Severity;

fn lint_line() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\[(INFO|WARNING|ERROR)\] ([^:]*?): (.*)$").expect("static regex"))
}

fn template_position() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"([A-Za-z0-9_./\-]+\.(?:ya?ml|tpl|json|txt|toml)):(\d+)(?::(\d+))?").expect("static regex")
    })
}

fn path_line() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^([^\s:][^:]*):(\d+)(?::(\d+))?: (.*)$").expect("static regex"))
}

/// Findings from helm's combined output. Lint lines and `Error:` lines are
/// recognised; the `==> Linting` banner, the summary and blank lines are not
/// findings.
pub(super) fn parse(stdout: &str, stderr: &str) -> Vec<Finding> {
    let mut findings = Vec::new();
    for line in stdout.lines().chain(stderr.lines()) {
        if let Some(captures) = lint_line().captures(line) {
            let severity = match &captures[1] {
                "ERROR" => Severity::Error,
                "WARNING" => Severity::Warning,
                _ => Severity::Info,
            };
            let mut finding = Finding::new(severity, captures[3].trim());
            finding.path = Some(captures[2].trim().to_owned());
            locate(&mut finding);
            findings.push(finding);
        } else if let Some(message) = line.strip_prefix("Error: ") {
            let mut finding = Finding::new(Severity::Error, message.trim());
            locate(&mut finding);
            findings.push(finding);
        } else if line.starts_with(' ')
            && !line.trim().is_empty()
            && let Some(last) = findings.last_mut()
        {
            last.message.push('\n');
            last.message.push_str(line.trim());
        }
    }
    findings
}

/// Pull a template position out of a finding's message when it has one and the
/// finding has no path of its own.
fn locate(finding: &mut Finding) {
    if finding.path.as_deref().is_some_and(|path| path.contains('.')) {
        return;
    }
    let Some(captures) = template_position().captures(&finding.message) else {
        return;
    };
    finding.path = Some(captures[1].to_owned());
    finding.line = captures[2].parse().ok();
    finding.column = captures.get(3).and_then(|m| m.as_str().parse().ok());
}

/// gcc-style `path:line[:col]: message` lines.
pub(super) fn path_lines(stdout: &str, stderr: &str) -> Vec<Finding> {
    stdout
        .lines()
        .chain(stderr.lines())
        .filter_map(|line| {
            let captures = path_line().captures(line)?;
            let mut finding = Finding::new(Severity::Error, captures[4].trim());
            finding.path = Some(captures[1].to_owned());
            finding.line = captures[2].parse().ok();
            finding.column = captures.get(3).and_then(|m| m.as_str().parse().ok());
            Some(finding)
        })
        .collect()
}
