//! `lazy-ignore`: a suppression directive written with no adjacent
//! justification, for tools **other than poly itself**.
//!
//! `filter/suppress.rs` (ADR 0028) already reports `lazy-ignore` (engine
//! `"poly"`) for an unjustified `poly: allow[…]`/`allow-file[…]` directive.
//! This rule covers the directives *nothing else* covers: `#[allow(…)]`
//! without a reason, bare `# noqa`, `// eslint-disable*`,
//! `// oxlint-disable*`, and `// biome-ignore`. It must never re-report
//! poly's own directive — the two scan for entirely different marker text
//! (`poly:` vs. these tool-specific markers), so a file with only an
//! unjustified `poly: allow[…]` produces exactly one `lazy-ignore` finding
//! (from `filter/suppress.rs`), not two. See `mod.rs` tests for the
//! regression check.
//!
//! `@ts-expect-error`/`@ts-ignore`/`@ts-nocheck` are deliberately **not**
//! scanned for: oxlint's `ban-ts-comment` (`pedantic`, on) already requires a
//! ≥3-character description on every one of them.
//!
//! # Detection, not parsing
//!
//! Like `filter/suppress.rs`, this is a line-oriented text scan, not a
//! tree-sitter parse — the same trade that keeps every language covered
//! without a per-language comment-syntax table or a parse on the hot path.

use crate::engine::{Diagnostic, Severity, Span};

/// How a marker's trailing text must be read to decide whether it justifies
/// the suppression.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReasonPolicy {
    /// The prefix is followed by a `<descriptor>: <reason>` shape (Biome's
    /// `// biome-ignore lint/x/y: reason`) — the reason is whatever comes
    /// after that *second* colon, not the rule descriptor itself.
    DescriptorThenReason,
    /// ESLint/oxlint have no formal reason syntax, but codebases that bother
    /// to explain a disable comment conventionally do it after a `--`
    /// separator (`// eslint-disable-next-line no-console -- reason`) — the
    /// rule-name list before it is never itself read as a reason.
    AfterDoubleDash,
    /// `#[allow(…)]`/`#![allow(…)]`: the lint names inside the parentheses
    /// (e.g. `dead_code`) are never themselves a reason — only a trailing
    /// same-line comment after the attribute's closing `)]` counts.
    AfterAttributeClose,
}

/// One marker this rule recognizes, and how its "reason" text is read.
struct Marker {
    /// Literal substring that opens the directive.
    prefix: &'static str,
    reason: ReasonPolicy,
    /// Whether `prefix` must be the very first thing on the trimmed line.
    ///
    /// A real `#[allow(…)]`/`#![allow(…)]` attribute is always the first
    /// token on its line — it can never appear after a `//` line-comment
    /// opener, because at that point it is inside the comment's text, not
    /// real code. Without this, a doc comment that merely *mentions*
    /// `#[allow(...)]` (exactly like this module's own docs and its
    /// fixture) is misread as an actual unjustified attribute. The
    /// comment-based markers (`noqa`, `eslint-disable`, …) do not get this
    /// restriction: those legitimately appear as a trailing comment after
    /// real code on the same line (`code(); // noqa`).
    at_line_start: bool,
}

const MARKERS: &[Marker] = &[
    Marker {
        prefix: "#[allow(",
        reason: ReasonPolicy::AfterAttributeClose,
        at_line_start: true,
    },
    Marker {
        prefix: "#![allow(",
        reason: ReasonPolicy::AfterAttributeClose,
        at_line_start: true,
    },
    Marker {
        prefix: "# noqa",
        reason: ReasonPolicy::DescriptorThenReason,
        at_line_start: false,
    },
    Marker {
        prefix: "// eslint-disable",
        reason: ReasonPolicy::AfterDoubleDash,
        at_line_start: false,
    },
    Marker {
        prefix: "// oxlint-disable",
        reason: ReasonPolicy::AfterDoubleDash,
        at_line_start: false,
    },
    Marker {
        prefix: "// biome-ignore",
        reason: ReasonPolicy::DescriptorThenReason,
        at_line_start: false,
    },
];

/// Minimum number of alphanumeric characters a trailing reason must contain
/// to count as a justification — mirrors `filter/suppress.rs`'s "at least
/// one alphanumeric character after `]`" rule, generalized slightly since
/// these directives are not bracket-delimited.
const MIN_REASON_ALNUM: usize = 3;

/// Scan `content` for unjustified suppression directives, returning one
/// `lazy-ignore` diagnostic per offending line.
pub fn scan(content: &str) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for (line_no, raw_line) in content.split('\n').enumerate() {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if let Some(finding) = scan_line(line, line_no) {
            out.push(finding);
        }
    }
    out
}

fn scan_line(line: &str, line_no: usize) -> Option<Diagnostic> {
    let trimmed = line.trim_start();
    let indent = line.len() - trimmed.len();

    for marker in MARKERS {
        let found = if marker.at_line_start {
            trimmed.strip_prefix(marker.prefix)
        } else {
            find_marker(trimmed, marker.prefix)
        };
        let Some(rest) = found else {
            continue;
        };
        if is_justified(rest, marker.reason) {
            continue;
        }
        let marker_col = indent + (trimmed.len() - rest.len() - marker.prefix.len());
        return Some(diagnostic(line_no, marker_col, marker.prefix.len()));
    }
    None
}

/// Locate `prefix` as a whole-directive opener within `trimmed` (either right
/// at the start of the trimmed line, so a same-line prefix like `code();
/// // eslint-disable-line` is still found after the code) and return the text
/// following it.
fn find_marker<'a>(trimmed: &'a str, prefix: &str) -> Option<&'a str> {
    let idx = trimmed.find(prefix)?;
    Some(&trimmed[idx + prefix.len()..])
}

/// Whether the text after a directive's prefix carries a real justification,
/// per the marker's [`ReasonPolicy`]. See the enum docs for what each variant
/// considers a reason.
fn is_justified(rest: &str, policy: ReasonPolicy) -> bool {
    let has_enough_alnum = |text: &str| text.chars().filter(|c| c.is_alphanumeric()).count() >= MIN_REASON_ALNUM;

    match policy {
        ReasonPolicy::DescriptorThenReason => match rest.split_once(':') {
            Some((_descriptor, reason)) => has_enough_alnum(reason),
            None => false,
        },
        ReasonPolicy::AfterDoubleDash => match rest.split_once("--") {
            Some((_rule_names, reason)) => has_enough_alnum(reason),
            None => false,
        },
        ReasonPolicy::AfterAttributeClose => match rest.split_once(")]") {
            Some((_lint_names, trailing)) => has_enough_alnum(trailing),
            None => false,
        },
    }
}

fn diagnostic(line_no: usize, col: usize, marker_len: usize) -> Diagnostic {
    let line = (line_no as u32) + 1;
    let col = (col as u32) + 1;
    Diagnostic {
        engine: "quality".to_owned(),
        code: Some("lazy-ignore".to_owned()),
        severity: Severity::Warning,
        title: "suppression directive has no justification".to_owned(),
        description: Some("add a short reason so a future reader knows why this warning is suppressed".to_owned()),
        span: Some(Span {
            start_line: line,
            start_col: col,
            end_line: line,
            end_col: col + marker_len as u32,
        }),
        url: None,
        fix: Vec::new(),
        metadata: std::collections::BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(diagnostics: &[Diagnostic]) -> Vec<&str> {
        diagnostics.iter().map(|d| d.code.as_deref().unwrap()).collect()
    }

    #[test]
    fn flags_bare_rust_allow() {
        let findings = scan("#[allow(dead_code)]\nfn f() {}\n");
        assert_eq!(codes(&findings), vec!["lazy-ignore"]);
    }

    #[test]
    fn does_not_flag_rust_allow_with_trailing_reason_comment() {
        let findings = scan("#[allow(dead_code)] // used only in benchmark builds\nfn f() {}\n");
        assert!(findings.is_empty());
    }

    #[test]
    fn flags_bare_noqa() {
        let findings = scan("import os  # noqa\n");
        assert_eq!(codes(&findings), vec!["lazy-ignore"]);
    }

    #[test]
    fn does_not_flag_noqa_with_a_reason() {
        let findings = scan("import os  # noqa: F401 re-exported for backwards compatibility\n");
        assert!(findings.is_empty());
    }

    #[test]
    fn flags_bare_eslint_disable() {
        let findings = scan("// eslint-disable-next-line\nconsole.log(1);\n");
        assert_eq!(codes(&findings), vec!["lazy-ignore"]);
    }

    #[test]
    fn does_not_flag_eslint_disable_with_a_double_dash_reason() {
        let findings =
            scan("// eslint-disable-next-line no-console -- intentional debug output in dev builds\nconsole.log(1);\n");
        assert!(findings.is_empty());
    }

    #[test]
    fn flags_bare_oxlint_disable() {
        let findings = scan("// oxlint-disable-next-line no-console\nconsole.log(1);\n");
        assert_eq!(codes(&findings), vec!["lazy-ignore"]);
    }

    #[test]
    fn flags_biome_ignore_with_no_reason() {
        let findings = scan("// biome-ignore lint/suspicious/noConsole:\nconsole.log(1);\n");
        assert_eq!(codes(&findings), vec!["lazy-ignore"]);
    }

    #[test]
    fn does_not_flag_biome_ignore_with_a_reason() {
        let findings = scan("// biome-ignore lint/suspicious/noConsole: intentional debug output\nconsole.log(1);\n");
        assert!(findings.is_empty());
    }

    /// Never re-report poly's own directive: this rule's marker set does not
    /// include `poly:`, so `filter/suppress.rs` is the only place an
    /// unjustified `poly: allow[…]` is ever reported.
    #[test]
    fn does_not_scan_polys_own_allow_directive() {
        let findings = scan("code(); // poly: allow[F401]\n");
        assert!(
            findings.is_empty(),
            "quality::lazy_ignore must not scan `poly:` directives"
        );
    }

    #[test]
    fn does_not_flag_ts_expect_error_left_to_oxlint() {
        let findings = scan("// @ts-expect-error\nconst x: number = \"nope\";\n");
        assert!(findings.is_empty());
    }

    /// A `//` comment that merely *mentions* `#[allow(...)]` in prose (this
    /// module's own docs do exactly this) must not be misread as a real,
    /// unjustified attribute — a real attribute can never appear after a
    /// line-comment opener.
    #[test]
    fn does_not_flag_allow_mentioned_inside_a_prose_comment() {
        let findings = scan("// See #[allow(dead_code)] for why this is kept.\nfn f() {}\n");
        assert!(findings.is_empty(), "{findings:?}");
    }
}
