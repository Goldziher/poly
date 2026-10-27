//! Parsers turning a tool's report into [`Finding`]s.
//!
//! One function per tool, all of the same shape: `(stdout, stderr)` in, a list
//! of findings out, or an `Err` when the output is not the document the tool is
//! known to print. The `Err` matters: a tool that crashed prints a usage
//! message, not a report, and reading *that* as "no findings" is a false pass.

mod checkstyle;
mod helm;
mod json;

use anyhow::Result;
use poly_catalog::Output;

use crate::engine::Severity;

/// One finding of a tool, before it is attributed to a file of the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::engines::catalog_tool) struct Finding {
    /// The file as the tool printed it (absolute, or relative to its cwd);
    /// `None` for a finding about the whole invocation.
    pub(in crate::engines::catalog_tool) path: Option<String>,
    pub(in crate::engines::catalog_tool) line: Option<u32>,
    pub(in crate::engines::catalog_tool) column: Option<u32>,
    pub(in crate::engines::catalog_tool) end_line: Option<u32>,
    pub(in crate::engines::catalog_tool) end_column: Option<u32>,
    pub(in crate::engines::catalog_tool) severity: Severity,
    /// The rule that fired.
    pub(in crate::engines::catalog_tool) code: Option<String>,
    pub(in crate::engines::catalog_tool) message: String,
    pub(in crate::engines::catalog_tool) url: Option<String>,
    /// The tool can fix this finding itself.
    pub(in crate::engines::catalog_tool) fixable: bool,
}

impl Finding {
    pub(super) fn new(severity: Severity, message: impl Into<String>) -> Self {
        Finding {
            path: None,
            line: None,
            column: None,
            end_line: None,
            end_column: None,
            severity,
            code: None,
            message: message.into(),
            url: None,
            fixable: false,
        }
    }
}

/// Parse `stdout`/`stderr` of a run of a tool whose report format is `format`.
///
/// # Errors
///
/// Returns `Err` when the output is not a report of that format.
pub(in crate::engines::catalog_tool) fn parse(format: Output, stdout: &str, stderr: &str) -> Result<Vec<Finding>> {
    match format {
        Output::Exit => Ok(Vec::new()),
        Output::CheckstyleXml => checkstyle::parse(stdout),
        Output::ActionlintJson => json::actionlint(stdout),
        Output::BufJson => json::buf(stdout),
        Output::HadolintJson => json::hadolint(stdout),
        Output::GolangciJson => json::golangci(stdout),
        Output::BuildifierJson => json::buildifier(stdout, stderr),
        Output::TflintJson => json::tflint(stdout),
        Output::GroovyJson => json::groovy(stdout),
        Output::HelmText => Ok(helm::parse(stdout, stderr)),
        Output::PathLine => Ok(helm::path_lines(stdout, stderr)),
    }
}

/// A 1-based line/column the tool printed, or `None` for the `0` some tools use
/// for "unknown".
pub(super) fn position(value: Option<u64>) -> Option<u32> {
    value.filter(|&n| n > 0).and_then(|n| u32::try_from(n).ok())
}

#[cfg(test)]
mod tests;
