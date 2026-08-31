//! Inline suppression directives (ADR 0028): `poly: allow[RULE] reason` and
//! `poly: allow-file[RULE] reason`, written in the host language's own comment
//! syntax and applied centrally in the runner so every engine inherits them.
//!
//! # Comment detection is a deliberate heuristic, not a parser
//!
//! Locating the directive does **not** parse the file. We find the literal
//! marker `poly:` in a line, take the text before it, trim trailing whitespace,
//! and accept the directive only when that prefix ends with a known comment
//! opener: `//`, `#`, `--`, `;`, `/*`, `*`, `<!--`, `%`, `!`, `dnl`, or a
//! case-insensitive `rem`. That covers every language poly routes without a
//! per-language table and without paying for a tree-sitter parse on the
//! per-file hot path.
//!
//! `"` and `'` are deliberately **not** openers, so a directive-shaped *string
//! literal* (`let s = "poly: allow[F401] nope";`) never suppresses. The
//! heuristic is shallow by construction: a comment opener that itself appears
//! inside a string literal (`"# poly: allow[…]"`) is not distinguished. That
//! residual case is accepted — the cost is one over-broad suppression in code
//! nobody writes by accident, whereas a real parse would cost a parse per file.
//!
//! # Scope
//!
//! - `allow-file` applies to the entire file wherever it appears.
//! - `allow` on a line that is *entirely* a comment applies to the next
//!   non-blank line.
//! - `allow` trailing after code applies to that same line.
//! - A diagnostic with no span is only suppressible by `allow-file`.
//!
//! # The reason is mandatory
//!
//! A directive suppresses only when the text after `]` contains at least one
//! alphanumeric character. An unjustified directive does **not** suppress and
//! instead reports `lazy-ignore`. Non-suppression (rather than the ADR's
//! "trades one finding for another") is the only safe reading: `poly lint`
//! exits non-zero only on `Error`, so a `Warning`-severity trade would let an
//! empty comment silently bypass CI.

use std::collections::BTreeMap;

use super::diagnostics::code_matches_rule;
use super::suppressed::{SuppressedDiagnostic, SuppressionReason};
use crate::engine::{Diagnostic, Severity, Span};

/// Literal that gates the whole mechanism. Every directive contains it, so a
/// file without it needs no line splitting at all.
const MARKER: &str = "poly:";

/// Directive keywords, longest first: `allow-file` must be tested before
/// `allow` or it would parse as `allow` followed by a stray `-file`.
const ALLOW_FILE: &str = "allow-file";
const ALLOW: &str = "allow";

/// Rule list that means "every rule".
const WILDCARD: &str = "*";

/// Comment openers accepted immediately before the marker. Quote characters are
/// excluded on purpose (see the module docs).
const OPENERS: &[&str] = &["//", "#", "--", ";", "/*", "*", "<!--", "%", "!", "dnl"];

/// Openers matched without regard to case (Batch `REM`/`rem`/`Rem`).
const CASE_INSENSITIVE_OPENERS: &[&str] = &["rem"];

/// Trailing comment terminators stripped from a reason so a block-comment
/// directive's reason is the prose, not `reason */`.
const TERMINATORS: &[&str] = &["*/", "-->"];

/// Code reported for a directive written without a justification.
const LAZY_IGNORE: &str = "lazy-ignore";

/// The set of rules one directive names.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RuleSet {
    /// `allow[*]` — every rule, including diagnostics that carry no code.
    All,
    /// An explicit, comma-separated list of rule codes.
    Codes(Vec<String>),
}

impl RuleSet {
    /// Parse the text between the brackets. Blank entries are dropped: an empty
    /// code would prefix-match every diagnostic and silently suppress the file.
    fn parse(list: &str) -> Self {
        let mut codes = Vec::new();
        for entry in list.split(',') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            if entry == WILDCARD {
                return Self::All;
            }
            codes.push(entry.to_owned());
        }
        Self::Codes(codes)
    }

    /// Whether this set covers `code`, using the same exact-or-prefix semantics
    /// `[per-file-ignores]` uses, so one rule spelling works in both places.
    fn matches(&self, code: Option<&str>) -> bool {
        match self {
            Self::All => true,
            Self::Codes(codes) => {
                let Some(code) = code else {
                    return false;
                };
                codes.iter().any(|rule| code_matches_rule(code, rule))
            }
        }
    }
}

/// Every inline directive found in one file's contents, compiled into the two
/// lookups the runner needs plus the findings unjustified directives produce.
///
/// Built once per file per lint pass — the fix loop rewrites the file, so line
/// numbers shift and the set must be rebuilt from the current content.
#[derive(Debug, Default)]
pub(crate) struct Suppressions {
    /// Rule sets from `allow-file` directives; each covers the whole file.
    file: Vec<RuleSet>,
    /// `(target_line, rules)` from `allow` directives, 1-based.
    lines: Vec<(u32, RuleSet)>,
    /// `lazy-ignore` findings for directives written without a reason.
    lazy: Vec<Diagnostic>,
}

impl Suppressions {
    /// Scan `content` for directives.
    ///
    /// The substring test is the hot-path gate: `lint_one` runs inside a rayon
    /// `par_iter` over every file in the repository, and almost none of them
    /// contain the marker. A file without it costs one linear scan and returns
    /// an empty value that allocates nothing.
    pub(crate) fn parse(content: &str) -> Self {
        if !content.contains(MARKER) {
            return Self::default();
        }
        let lines: Vec<&str> = content.lines().collect();
        let mut suppressions = Self::default();
        for (index, line) in lines.iter().enumerate() {
            let Some(directive) = Directive::parse(line) else {
                continue;
            };
            // Reported on the directive's own line: that is where the fix goes.
            let line_number = index as u32 + 1;
            if !directive.justified {
                suppressions.lazy.push(lazy_ignore(line, line_number, directive.column));
                continue;
            }
            match directive.scope {
                Scope::File => suppressions.file.push(directive.rules),
                Scope::NextLine => {
                    if let Some(target) = next_non_blank(&lines, index) {
                        suppressions.lines.push((target, directive.rules));
                    }
                }
                Scope::SameLine => suppressions.lines.push((line_number, directive.rules)),
            }
        }
        suppressions
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.file.is_empty() && self.lines.is_empty() && self.lazy.is_empty()
    }

    /// Drop suppressed diagnostics — recording each in `suppressed` — then
    /// append the `lazy-ignore` findings.
    ///
    /// Appending after the retain is what keeps a directive from suppressing the
    /// very finding it produced — or any other directive's. The runner applies
    /// `[per-file-ignores]` *after* this call so a `lazy-ignore` is still
    /// silenceable by config; the two retains are independent filters, so their
    /// relative order does not change which real diagnostics survive.
    ///
    /// A `lazy-ignore` is appended, not suppressed, so it never appears in
    /// `suppressed`: the directive that produced it is inert by definition and
    /// dropped nothing.
    pub(crate) fn apply(
        &self,
        path: &std::path::Path,
        diagnostics: &mut Vec<Diagnostic>,
        suppressed: &mut Vec<SuppressedDiagnostic>,
    ) {
        if !self.file.is_empty() || !self.lines.is_empty() {
            diagnostics.retain(|diagnostic| {
                if self.suppresses(diagnostic) {
                    suppressed.push(SuppressedDiagnostic::new(
                        path,
                        diagnostic,
                        SuppressionReason::InlineSuppression,
                    ));
                    return false;
                }
                true
            });
        }
        diagnostics.extend(self.lazy.iter().cloned());
    }

    fn suppresses(&self, diagnostic: &Diagnostic) -> bool {
        let code = diagnostic.code.as_deref();
        if self.file.iter().any(|rules| rules.matches(code)) {
            return true;
        }
        let Some(span) = diagnostic.span else {
            return false;
        };
        self.lines
            .iter()
            .any(|(line, rules)| *line == span.start_line && rules.matches(code))
    }
}

/// Where a parsed directive applies.
#[derive(Debug, PartialEq, Eq)]
enum Scope {
    /// `allow-file`.
    File,
    /// `allow` on a comment-only line.
    NextLine,
    /// `allow` trailing after code.
    SameLine,
}

/// One directive, already validated against the comment-opener heuristic.
#[derive(Debug)]
struct Directive {
    scope: Scope,
    rules: RuleSet,
    /// Whether the reason text carries at least one alphanumeric character.
    justified: bool,
    /// 1-based column of the marker, for the `lazy-ignore` span.
    column: u32,
}

impl Directive {
    /// Parse the first directive on `line`, or `None` when the line has no
    /// marker preceded by a comment opener, or the marker is not followed by a
    /// well-formed `allow[...]` / `allow-file[...]`.
    fn parse(line: &str) -> Option<Self> {
        for (offset, _) in line.match_indices(MARKER) {
            let prefix = &line[..offset];
            if !ends_with_comment_opener(prefix) {
                continue;
            }
            let rest = line[offset + MARKER.len()..].trim_start();
            let (scope, rest) = if let Some(rest) = rest.strip_prefix(ALLOW_FILE) {
                (Scope::File, rest)
            } else if let Some(rest) = rest.strip_prefix(ALLOW) {
                let scope = if starts_with_comment_opener(line.trim_start()) {
                    Scope::NextLine
                } else {
                    Scope::SameLine
                };
                (scope, rest)
            } else {
                continue;
            };
            // The bracketed rule list is required. A directive written without
            // it (`poly: allow F401`) is not recognized and suppresses nothing.
            let Some(rest) = rest.trim_start().strip_prefix('[') else {
                continue;
            };
            let Some((list, reason)) = rest.split_once(']') else {
                continue;
            };
            return Some(Self {
                scope,
                rules: RuleSet::parse(list),
                justified: is_justified(reason),
                column: prefix.chars().count() as u32 + 1,
            });
        }
        None
    }
}

/// A reason justifies a directive only when it carries real text. Trailing
/// block-comment terminators are stripped first, so `/* poly: allow[X] */` is
/// correctly seen as having no reason at all.
fn is_justified(reason: &str) -> bool {
    let mut reason = reason.trim();
    for terminator in TERMINATORS {
        reason = reason.strip_suffix(terminator).unwrap_or(reason).trim_end();
    }
    reason.chars().any(char::is_alphanumeric)
}

/// Whether the text immediately before the marker ends with a comment opener.
fn ends_with_comment_opener(prefix: &str) -> bool {
    let prefix = prefix.trim_end();
    OPENERS.iter().any(|opener| prefix.ends_with(opener))
        || CASE_INSENSITIVE_OPENERS
            .iter()
            .any(|opener| ends_with_ignore_ascii_case(prefix, opener))
}

/// Whether a line begins with a comment opener — i.e. the whole line is a
/// comment, so its directive targets the next non-blank line rather than itself.
fn starts_with_comment_opener(trimmed: &str) -> bool {
    OPENERS.iter().any(|opener| trimmed.starts_with(opener))
        || CASE_INSENSITIVE_OPENERS.iter().any(|opener| {
            trimmed
                .get(..opener.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(opener))
        })
}

fn ends_with_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    haystack
        .len()
        .checked_sub(needle.len())
        .and_then(|start| haystack.get(start..))
        .is_some_and(|tail| tail.eq_ignore_ascii_case(needle))
}

/// 1-based number of the first non-blank line after `index`, if any.
fn next_non_blank(lines: &[&str], index: usize) -> Option<u32> {
    lines
        .iter()
        .enumerate()
        .skip(index + 1)
        .find(|(_, line)| !line.trim().is_empty())
        .map(|(found, _)| found as u32 + 1)
}

/// The finding an unjustified directive produces in place of the suppression it
/// asked for.
fn lazy_ignore(line: &str, line_number: u32, column: u32) -> Diagnostic {
    Diagnostic {
        engine: "poly".to_owned(),
        code: Some(LAZY_IGNORE.to_owned()),
        severity: Severity::Warning,
        title: "suppression ignored: this poly directive carries no reason".to_owned(),
        description: Some(
            "A `poly: allow[…]` / `poly: allow-file[…]` directive must state why the rule is \
             suppressed. Without a reason the directive is inert and the rule still applies — \
             add the justification after the closing bracket."
                .to_owned(),
        ),
        span: Some(Span {
            start_line: line_number,
            start_col: column,
            end_line: line_number,
            end_col: line.chars().count() as u32 + 1,
        }),
        url: None,
        fix: Vec::new(),
        metadata: BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diag(code: Option<&str>, line: Option<u32>) -> Diagnostic {
        Diagnostic {
            engine: "test".to_owned(),
            code: code.map(str::to_owned),
            severity: Severity::Error,
            title: "x".to_owned(),
            description: None,
            span: line.map(|start_line| Span {
                start_line,
                start_col: 1,
                end_line: start_line,
                end_col: 2,
            }),
            url: None,
            fix: Vec::new(),
            metadata: BTreeMap::new(),
        }
    }

    fn codes(diagnostics: &[Diagnostic]) -> Vec<Option<&str>> {
        diagnostics.iter().map(|d| d.code.as_deref()).collect()
    }

    /// The file these unit tests report against. Only carried through to the
    /// suppression record; nothing here matches on it.
    fn test_path() -> &'static std::path::Path {
        std::path::Path::new("src/app.py")
    }

    #[test]
    fn a_file_without_the_marker_parses_to_an_empty_set() {
        let suppressions = Suppressions::parse("fn main() {}\n");
        assert!(suppressions.is_empty());
        let mut diagnostics = vec![diag(Some("F401"), Some(1))];
        suppressions.apply(test_path(), &mut diagnostics, &mut Vec::new());
        assert_eq!(codes(&diagnostics), vec![Some("F401")]);
    }

    #[test]
    fn trailing_directive_suppresses_its_own_line() {
        let suppressions = Suppressions::parse("import os  # poly: allow[F401] kept for re-export\n");
        let mut diagnostics = vec![diag(Some("F401"), Some(1)), diag(Some("E501"), Some(1))];
        suppressions.apply(test_path(), &mut diagnostics, &mut Vec::new());
        assert_eq!(
            codes(&diagnostics),
            vec![Some("E501")],
            "only the named rule is dropped"
        );
    }

    #[test]
    fn comment_only_directive_suppresses_the_next_non_blank_line() {
        let source = "# poly: allow[F401] re-exported on purpose\n\n\nimport os\n";
        let suppressions = Suppressions::parse(source);
        let mut diagnostics = vec![diag(Some("F401"), Some(4)), diag(Some("F401"), Some(1))];
        suppressions.apply(test_path(), &mut diagnostics, &mut Vec::new());
        assert_eq!(
            codes(&diagnostics),
            vec![Some("F401")],
            "blank lines are skipped; only line 4 is covered"
        );
        assert_eq!(diagnostics[0].span.unwrap().start_line, 1, "the directive's own line");
    }

    #[test]
    fn comment_only_directive_at_end_of_file_targets_nothing() {
        let suppressions = Suppressions::parse("import os\n# poly: allow[F401] nothing follows\n\n");
        let mut diagnostics = vec![diag(Some("F401"), Some(1))];
        suppressions.apply(test_path(), &mut diagnostics, &mut Vec::new());
        assert_eq!(codes(&diagnostics), vec![Some("F401")]);
    }

    #[test]
    fn allow_file_suppresses_a_diagnostic_with_no_span() {
        let suppressions = Suppressions::parse("// poly: allow-file[no-console] debug entrypoint\ncode();\n");
        let mut diagnostics = vec![diag(Some("no-console"), None), diag(Some("no-console"), Some(2))];
        suppressions.apply(test_path(), &mut diagnostics, &mut Vec::new());
        assert!(diagnostics.is_empty(), "span-less and spanned alike are suppressed");
    }

    #[test]
    fn a_line_scoped_directive_never_suppresses_a_span_less_diagnostic() {
        let suppressions = Suppressions::parse("code(); // poly: allow[no-console] intentional\n");
        let mut diagnostics = vec![diag(Some("no-console"), None)];
        suppressions.apply(test_path(), &mut diagnostics, &mut Vec::new());
        assert_eq!(codes(&diagnostics), vec![Some("no-console")]);
    }

    #[test]
    fn an_unjustified_directive_does_not_suppress_and_reports_lazy_ignore() {
        let suppressions = Suppressions::parse("import os  # poly: allow[F401]\n");
        let mut diagnostics = vec![diag(Some("F401"), Some(1))];
        suppressions.apply(test_path(), &mut diagnostics, &mut Vec::new());

        assert_eq!(
            codes(&diagnostics),
            vec![Some("F401"), Some("lazy-ignore")],
            "the rule still fires and the directive itself is reported"
        );
        let lazy = &diagnostics[1];
        assert_eq!(lazy.engine, "poly");
        assert_eq!(lazy.severity, Severity::Warning);
        assert_eq!(lazy.span.unwrap().start_line, 1);
    }

    #[test]
    fn an_empty_block_comment_reason_is_unjustified() {
        let suppressions = Suppressions::parse("code(); /* poly: allow[no-console] */\n");
        let mut diagnostics = vec![diag(Some("no-console"), Some(1))];
        suppressions.apply(test_path(), &mut diagnostics, &mut Vec::new());
        assert_eq!(codes(&diagnostics), vec![Some("no-console"), Some("lazy-ignore")]);
    }

    #[test]
    fn a_block_comment_reason_survives_its_terminator() {
        let suppressions = Suppressions::parse("code(); /* poly: allow[no-console] debug shim */\n");
        let mut diagnostics = vec![diag(Some("no-console"), Some(1))];
        suppressions.apply(test_path(), &mut diagnostics, &mut Vec::new());
        assert!(diagnostics.is_empty(), "a real reason justifies the directive");
    }

    #[test]
    fn punctuation_alone_is_not_a_reason() {
        let suppressions = Suppressions::parse("code(); // poly: allow[no-console] ---\n");
        let mut diagnostics = vec![diag(Some("no-console"), Some(1))];
        suppressions.apply(test_path(), &mut diagnostics, &mut Vec::new());
        assert_eq!(codes(&diagnostics), vec![Some("no-console"), Some("lazy-ignore")]);
    }

    #[test]
    fn a_directive_inside_a_string_literal_does_not_suppress() {
        let source = "let s = \"poly: allow[no-console] nope\";\ncode();\n";
        let suppressions = Suppressions::parse(source);
        assert!(suppressions.is_empty(), "a quote is not a comment opener");
        let mut diagnostics = vec![diag(Some("no-console"), Some(1)), diag(Some("no-console"), Some(2))];
        suppressions.apply(test_path(), &mut diagnostics, &mut Vec::new());
        assert_eq!(diagnostics.len(), 2, "nothing is suppressed");
    }

    #[test]
    fn wildcard_suppresses_every_rule_including_code_less_diagnostics() {
        let suppressions = Suppressions::parse("code(); // poly: allow[*] generated shim\n");
        let mut diagnostics = vec![
            diag(Some("no-console"), Some(1)),
            diag(None, Some(1)),
            diag(Some("x"), Some(2)),
        ];
        suppressions.apply(test_path(), &mut diagnostics, &mut Vec::new());
        assert_eq!(codes(&diagnostics), vec![Some("x")], "only line 1 is covered");
    }

    #[test]
    fn a_multi_rule_list_is_comma_separated_and_whitespace_tolerant() {
        let suppressions = Suppressions::parse("code(); // poly: allow[ F401 ,no-console , ] two rules\n");
        let mut diagnostics = vec![
            diag(Some("F401"), Some(1)),
            diag(Some("no-console"), Some(1)),
            diag(Some("E501"), Some(1)),
        ];
        suppressions.apply(test_path(), &mut diagnostics, &mut Vec::new());
        assert_eq!(codes(&diagnostics), vec![Some("E501")]);
    }

    #[test]
    fn rule_matching_is_prefix_bounded_like_per_file_ignores() {
        let suppressions = Suppressions::parse("code(); # poly: allow[F] the whole F family\n");
        let mut diagnostics = vec![diag(Some("F401"), Some(1)), diag(Some("FOO1"), Some(1))];
        suppressions.apply(test_path(), &mut diagnostics, &mut Vec::new());
        assert_eq!(
            codes(&diagnostics),
            vec![Some("FOO1")],
            "an alphabetic boundary blocks the prefix"
        );
    }

    #[test]
    fn an_empty_rule_list_is_not_a_wildcard() {
        let suppressions = Suppressions::parse("code(); // poly: allow[] a reason but no rules\n");
        let mut diagnostics = vec![diag(Some("F401"), Some(1))];
        suppressions.apply(test_path(), &mut diagnostics, &mut Vec::new());
        assert_eq!(codes(&diagnostics), vec![Some("F401")]);
    }

    #[test]
    fn a_directive_without_brackets_is_not_recognized() {
        let suppressions = Suppressions::parse("code(); // poly: allow F401 no brackets\n");
        assert!(suppressions.is_empty(), "no suppression and no lazy-ignore");
    }

    #[test]
    fn every_comment_opener_is_recognized() {
        let openers = [
            "//", "#", "--", ";", "/*", "*", "<!--", "%", "!", "dnl", "rem", "REM", "Rem",
        ];
        for opener in openers {
            let source = format!("code(); {opener} poly: allow[X] because\n");
            let mut diagnostics = vec![diag(Some("X"), Some(1))];
            Suppressions::parse(&source).apply(test_path(), &mut diagnostics, &mut Vec::new());
            assert!(diagnostics.is_empty(), "opener {opener:?} must be recognized");
        }
    }

    #[test]
    fn allow_file_applies_wherever_it_appears() {
        let source = "code();\ncode();\n-- poly: allow-file[LT01] vendored SQL\n";
        let mut diagnostics = vec![diag(Some("LT01"), Some(1)), diag(Some("LT01"), Some(2))];
        Suppressions::parse(source).apply(test_path(), &mut diagnostics, &mut Vec::new());
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn an_unjustified_allow_file_reports_exactly_one_lazy_ignore() {
        let suppressions = Suppressions::parse("# poly: allow-file[F401]\nimport os\n");
        let mut diagnostics = vec![diag(Some("F401"), Some(2))];
        suppressions.apply(test_path(), &mut diagnostics, &mut Vec::new());
        assert_eq!(codes(&diagnostics), vec![Some("F401"), Some("lazy-ignore")]);
        assert_eq!(
            diagnostics
                .iter()
                .filter(|d| d.code.as_deref() == Some("lazy-ignore"))
                .count(),
            1
        );
    }

    #[test]
    fn a_lazy_ignore_is_not_suppressible_by_another_directive() {
        let source = "# poly: allow-file[lazy-ignore] trying to silence the guard rail\ncode(); // poly: allow[X]\n";
        let mut diagnostics = Vec::new();
        Suppressions::parse(source).apply(test_path(), &mut diagnostics, &mut Vec::new());
        assert_eq!(codes(&diagnostics), vec![Some("lazy-ignore")]);
    }
}
