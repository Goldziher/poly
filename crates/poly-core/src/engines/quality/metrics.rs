//! `file-too-long`, `function-too-long`, `type-too-long`, and
//! `too-many-parameters`: the simple size/count metrics, plus the diagnostic
//! builders shared with `nesting`/`complexity` findings.

use tree_sitter::Node;

use super::definitions::{self, DefKind};
use super::settings::Settings;
use crate::engine::{Diagnostic, SourceFile};
use crate::language::Language;

/// `file-too-long`: a pure line count, so it applies to every language
/// regardless of parse support (never deferred — see `family` module docs).
pub fn check_file_too_long(src: &SourceFile, threshold: i64, out: &mut Vec<Diagnostic>) {
    let lines = count_lines(&src.content);
    if (lines as i64) <= threshold {
        return;
    }
    out.push(diagnostic(
        "file-too-long",
        format!("file is {lines} lines (max {threshold})"),
        1,
        1,
        lines as u32,
        1,
    ));
}

fn count_lines(content: &str) -> usize {
    if content.is_empty() {
        return 0;
    }
    let newlines = content.bytes().filter(|&b| b == b'\n').count();
    if content.ends_with('\n') {
        newlines
    } else {
        newlines + 1
    }
}

/// `function-too-long`, `type-too-long`, and `too-many-parameters`: all three
/// come from the same Tags-query definition list, so they are computed
/// together per file.
pub fn check_definitions(
    grammar: &str,
    root: Node,
    source: &[u8],
    language: &Language,
    settings: &Settings,
    out: &mut Vec<Diagnostic>,
) {
    let need_function = settings.function_too_long.enabled
        || (settings.too_many_parameters.enabled
            && !super::family::is_deferred(super::family::Rule::TooManyParameters, language));
    let need_type = settings.type_too_long.enabled;
    if !need_function && !need_type {
        return;
    }

    let Some(defs) = definitions::collect(grammar, root, source) else {
        return;
    };

    for def in defs {
        let lines = def.end_row - def.start_row + 1;
        match def.kind {
            DefKind::Function => {
                if settings.function_too_long.enabled && (lines as i64) > settings.function_too_long.threshold {
                    out.push(diagnostic(
                        "function-too-long",
                        format!(
                            "function is {lines} lines (max {})",
                            settings.function_too_long.threshold
                        ),
                        (def.start_row + 1) as u32,
                        1,
                        (def.end_row + 1) as u32,
                        1,
                    ));
                }
                if settings.too_many_parameters.enabled
                    && !super::family::is_deferred(super::family::Rule::TooManyParameters, language)
                    && let Some(count) = def.param_count
                    && (count as i64) > settings.too_many_parameters.threshold
                {
                    out.push(diagnostic(
                        "too-many-parameters",
                        format!(
                            "function has {count} parameters (max {})",
                            settings.too_many_parameters.threshold
                        ),
                        (def.start_row + 1) as u32,
                        1,
                        (def.start_row + 1) as u32,
                        1,
                    ));
                }
            }
            DefKind::Type => {
                if settings.type_too_long.enabled && (lines as i64) > settings.type_too_long.threshold {
                    out.push(diagnostic(
                        "type-too-long",
                        format!("type is {lines} lines (max {})", settings.type_too_long.threshold),
                        (def.start_row + 1) as u32,
                        1,
                        (def.end_row + 1) as u32,
                        1,
                    ));
                }
            }
        }
    }
}

/// `nesting::NestingFinding` only carries byte offsets (a whole-tree walk has
/// no reason to track rows), so this converts to a 1-based [`Span`] against
/// `content` the same way `engines/uncomment.rs::span_of` does.
pub fn nesting_diagnostic(finding: &super::nesting::NestingFinding, content: &str) -> Diagnostic {
    Diagnostic {
        engine: "quality".to_owned(),
        code: Some("nesting-too-deep".to_owned()),
        severity: crate::engine::Severity::Warning,
        title: format!("nesting depth is {} (max exceeded)", finding.depth),
        description: Some("extract a helper function to flatten this branch".to_owned()),
        span: Some(span_of(content, finding.start_byte, finding.end_byte)),
        url: None,
        fix: Vec::new(),
        metadata: std::collections::BTreeMap::new(),
    }
}

pub fn complexity_diagnostic(finding: &super::complexity::ComplexityFinding, content: &str) -> Diagnostic {
    Diagnostic {
        engine: "quality".to_owned(),
        code: Some("cyclomatic-complexity".to_owned()),
        severity: crate::engine::Severity::Warning,
        title: format!("cyclomatic complexity is {}", finding.complexity),
        description: Some("split this function into smaller pieces".to_owned()),
        span: Some(span_of(content, finding.start_byte, finding.end_byte)),
        url: None,
        fix: Vec::new(),
        metadata: std::collections::BTreeMap::new(),
    }
}

/// Build a 1-based [`crate::engine::Span`] covering the byte range
/// `[start, end)` of `content`. Mirrors `engines/uncomment.rs::span_of`.
fn span_of(content: &str, start: usize, end: usize) -> crate::engine::Span {
    let (start_line, start_col) = line_col(content, start);
    let (end_line, end_col) = line_col(content, end);
    crate::engine::Span {
        start_line,
        start_col,
        end_line,
        end_col,
    }
}

/// Convert a byte offset into `content` to a 1-based (line, column) pair,
/// counted in bytes. Mirrors `engines/uncomment.rs::line_col`.
fn line_col(content: &str, offset: usize) -> (u32, u32) {
    let offset = offset.min(content.len());
    let mut line: u32 = 1;
    let mut col: u32 = 1;
    for &byte in &content.as_bytes()[..offset] {
        if byte == b'\n' {
            line = line.saturating_add(1);
            col = 1;
        } else {
            col = col.saturating_add(1);
        }
    }
    (line, col)
}

fn diagnostic(code: &str, title: String, start_line: u32, start_col: u32, end_line: u32, end_col: u32) -> Diagnostic {
    Diagnostic {
        engine: "quality".to_owned(),
        code: Some(code.to_owned()),
        severity: crate::engine::Severity::Warning,
        title,
        description: None,
        span: Some(crate::engine::Span {
            start_line,
            start_col,
            end_line,
            end_col,
        }),
        url: None,
        fix: Vec::new(),
        metadata: std::collections::BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn file(content: &str) -> SourceFile {
        SourceFile {
            path: PathBuf::from("f.txt"),
            language: Language::Other("txt".to_owned()),
            content: Arc::from(content),
        }
    }

    #[test]
    fn counts_lines_with_and_without_trailing_newline() {
        assert_eq!(count_lines(""), 0);
        assert_eq!(count_lines("a\n"), 1);
        assert_eq!(count_lines("a\nb\n"), 2);
        assert_eq!(count_lines("a\nb"), 2);
    }

    #[test]
    fn flags_a_file_over_the_threshold() {
        let content = "x\n".repeat(5);
        let src = file(&content);
        let mut out = Vec::new();
        check_file_too_long(&src, 3, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].code.as_deref(), Some("file-too-long"));
    }

    #[test]
    fn does_not_flag_a_file_at_the_threshold() {
        let content = "x\n".repeat(3);
        let src = file(&content);
        let mut out = Vec::new();
        check_file_too_long(&src, 3, &mut out);
        assert!(
            out.is_empty(),
            "strict > threshold, so exactly-at-threshold must not fire"
        );
    }
}
