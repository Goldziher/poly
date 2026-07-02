//! Per-file lint dispatch for shellcheck: pipe source content through
//! `shellcheck --format=json1 -` and parse its JSON1 output into normalized
//! [`Diagnostic`]s.
//!
//! ## Shellcheck JSON1 schema
//!
//! ```json
//! {
//!   "comments": [
//!     {
//!       "file":      "-",
//!       "line":      3,
//!       "endLine":   3,
//!       "column":    6,
//!       "endColumn": 8,
//!       "level":     "info",
//!       "code":      2086,
//!       "message":   "Double quote to prevent globbing and word splitting.",
//!       "fix":       null
//!     }
//!   ]
//! }
//! ```
//!
//! The `"file"` field is always `"-"` (stdin) and is ignored. `"fix"` may
//! hold a replacement object; we do not currently map it to [`Edit`]s
//! (recorded for a follow-up).
//!
//! ## Exit codes
//!
//! | Code | Meaning |
//! |------|---------|
//! | 0    | No issues found. |
//! | 1    | Issues found (JSON output is still valid). |
//! | 2+   | Tool error (bad args, unreadable file, etc.). |
//!
//! We parse the output for exit codes 0 and 1; on 2+, we return an empty
//! `Vec` (graceful degradation — a broken shellcheck invocation is never a
//! hard error).

use std::io::Write;
use std::process::{Command, Stdio};
use std::thread;

use anyhow::Context;
use serde::Deserialize;

use crate::engine::{Diagnostic, Severity, SourceFile, Span};

use super::spec::ToolSpec;

// ---------------------------------------------------------------------------
// Deserialization types for the JSON1 format
// ---------------------------------------------------------------------------

/// Top-level shellcheck JSON1 response object.
#[derive(Debug, Deserialize)]
struct ShellcheckOutput {
    comments: Vec<ShellcheckComment>,
}

/// One shellcheck finding.
#[derive(Debug, Deserialize)]
struct ShellcheckComment {
    /// 1-based start line.
    line: u32,
    /// 1-based end line.
    #[serde(rename = "endLine")]
    end_line: u32,
    /// 1-based start column.
    column: u32,
    /// 1-based end column (exclusive — shellcheck convention).
    #[serde(rename = "endColumn")]
    end_column: u32,
    /// One of `"error"`, `"warning"`, `"info"`, `"style"`.
    level: String,
    /// Numeric SC code (e.g. 2086).
    code: u32,
    /// Human-readable description.
    message: String,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn shellcheck_level_to_severity(level: &str) -> Severity {
    match level {
        "error" => Severity::Error,
        "warning" => Severity::Warning,
        "info" => Severity::Info,
        "style" => Severity::Hint,
        _ => Severity::Warning, // defensive: unknown levels treated as warnings
    }
}

// ---------------------------------------------------------------------------
// Public surface
// ---------------------------------------------------------------------------

/// Parse shellcheck's `--format=json1` stdout into normalized [`Diagnostic`]s.
///
/// `engine_name` is stamped onto each diagnostic's `engine` field (the
/// string `"shellcheck"`).
///
/// This function is **pure** — it does not spawn any process — and is the
/// primary unit-test surface for the JSON→Diagnostic mapping.
pub(crate) fn parse_shellcheck_json(engine_name: &str, json: &str) -> anyhow::Result<Vec<Diagnostic>> {
    let output: ShellcheckOutput = serde_json::from_str(json).context("failed to parse shellcheck JSON1 output")?;

    let diags = output
        .comments
        .into_iter()
        .map(|c| Diagnostic {
            engine: engine_name.to_owned(),
            code: Some(format!("SC{}", c.code)),
            severity: shellcheck_level_to_severity(&c.level),
            title: c.message,
            description: None,
            url: None,
            span: Some(Span {
                start_line: c.line,
                start_col: c.column,
                end_line: c.end_line,
                end_col: c.end_column,
            }),
            fix: Vec::new(),
            metadata: Default::default(),
        })
        .collect();

    Ok(diags)
}

/// Pipe `src.content` through `shellcheck --format=json1 -` and parse the
/// JSON1 output into diagnostics.
///
/// Returns an empty `Vec` (not an error) when:
/// - shellcheck exits with code 2+ (tool error / bad invocation)
/// - shellcheck exits with code 0 (no issues found)
///
/// This function follows the same stdin-write-thread / wait_with_output
/// pattern as `format_via_tool` to prevent pipe-buffer deadlocks on large
/// files.
pub(crate) fn lint_via_shellcheck(spec: &ToolSpec, src: &SourceFile) -> anyhow::Result<Vec<Diagnostic>> {
    let lint_binary = spec
        .lint_binary
        .expect("lint_via_shellcheck called on a format-only ToolSpec");

    let mut child = Command::new(lint_binary)
        .args(spec.lint_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // Suppress tool stderr: exit code and JSON stdout are the signals.
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to spawn '{lint_binary}'"))?;

    let content = std::sync::Arc::clone(&src.content);
    let mut stdin_handle = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("'{lint_binary}' stdin pipe was not created"))?;

    let write_thread = thread::spawn(move || -> std::io::Result<()> {
        stdin_handle.write_all(content.as_bytes())
        // stdin_handle dropped here → EOF sent to shellcheck.
    });

    let output = child
        .wait_with_output()
        .with_context(|| format!("'{lint_binary}' wait_with_output failed"))?;

    // Always reap the write thread. For shellcheck:
    // - exit 0/1: shellcheck consumed all stdin, the write succeeded.
    // - exit 2+: shellcheck may have closed stdin early (broken pipe in the
    //   write thread). Either way we discard the write-thread error gracefully.
    let _ = write_thread.join();

    let exit_code = output.status.code().unwrap_or(2);
    if exit_code >= 2 {
        // Tool error (bad options, unreadable file, etc.). Return nothing —
        // a broken shellcheck invocation is never a hard lint error.
        return Ok(Vec::new());
    }

    // Exit 0 or 1: JSON output is valid regardless of the exit code.
    let json =
        String::from_utf8(output.stdout).with_context(|| format!("'{lint_binary}' produced non-UTF-8 output"))?;

    parse_shellcheck_json(spec.engine_name, &json)
}

// ---------------------------------------------------------------------------
// Unit tests (host-independent: no shellcheck binary required)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse a captured shellcheck JSON1 sample and verify the diagnostic
    /// mapping. Does NOT require shellcheck to be installed.
    #[test]
    fn parse_info_sc2086() {
        const SAMPLE: &str = r#"{"comments":[{"file":"-","line":3,"endLine":3,"column":6,"endColumn":8,"level":"info","code":2086,"message":"Double quote to prevent globbing and word splitting.","fix":null}]}"#;
        let diags = parse_shellcheck_json("shellcheck", SAMPLE).unwrap();
        assert_eq!(diags.len(), 1);
        let d = &diags[0];
        assert_eq!(d.engine, "shellcheck");
        assert_eq!(d.code.as_deref(), Some("SC2086"));
        assert_eq!(d.severity, Severity::Info);
        assert_eq!(d.title, "Double quote to prevent globbing and word splitting.");
        let span = d.span.unwrap();
        assert_eq!(span.start_line, 3);
        assert_eq!(span.start_col, 6);
        assert_eq!(span.end_line, 3);
        assert_eq!(span.end_col, 8);
        assert!(d.fix.is_empty(), "no fix edits in the parsed output");
    }

    /// Multiple findings at different severity levels are all parsed.
    #[test]
    fn parse_multiple_findings_at_different_severities() {
        const SAMPLE: &str = r#"{"comments":[
            {"file":"-","line":2,"endLine":2,"column":1,"endColumn":1,"level":"error","code":2034,"message":"x appears unused.","fix":null},
            {"file":"-","line":5,"endLine":5,"column":4,"endColumn":6,"level":"warning","code":2154,"message":"a is referenced but not assigned.","fix":null},
            {"file":"-","line":7,"endLine":7,"column":1,"endColumn":5,"level":"style","code":2250,"message":"Prefer putting braces around variable references.","fix":null}
        ]}"#;
        let diags = parse_shellcheck_json("shellcheck", SAMPLE).unwrap();
        assert_eq!(diags.len(), 3);
        assert_eq!(diags[0].severity, Severity::Error);
        assert_eq!(diags[0].code.as_deref(), Some("SC2034"));
        assert_eq!(diags[1].severity, Severity::Warning);
        assert_eq!(diags[1].code.as_deref(), Some("SC2154"));
        assert_eq!(diags[2].severity, Severity::Hint);
        assert_eq!(diags[2].code.as_deref(), Some("SC2250"));
    }

    /// An empty `comments` array (no findings) produces an empty Vec.
    #[test]
    fn parse_empty_comments_produces_no_diags() {
        let diags = parse_shellcheck_json("shellcheck", r#"{"comments":[]}"#).unwrap();
        assert!(diags.is_empty());
    }

    /// Malformed JSON returns an error.
    #[test]
    fn parse_malformed_json_returns_error() {
        assert!(parse_shellcheck_json("shellcheck", "not json").is_err());
    }

    /// Unknown `level` values are mapped to `Warning` rather than panicking.
    #[test]
    fn unknown_level_maps_to_warning() {
        const SAMPLE: &str = r#"{"comments":[{"file":"-","line":1,"endLine":1,"column":1,"endColumn":1,"level":"unknown_level","code":9999,"message":"test","fix":null}]}"#;
        let diags = parse_shellcheck_json("shellcheck", SAMPLE).unwrap();
        assert_eq!(diags[0].severity, Severity::Warning);
    }
}
