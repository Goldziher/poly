//! `lazy-ignore`: a suppression directive written with no adjacent
//! justification, for tools **other than poly itself**.
//!
//! `filter/suppress.rs` (ADR 0028) already reports `lazy-ignore` (engine
//! `"poly"`) for an unjustified `poly: allow[…]`/`allow-file[…]` directive.
//! This rule covers the directives *nothing else* covers: bare `# noqa`,
//! `// eslint-disable*`, `// oxlint-disable*`, and `// biome-ignore`. It
//! must never re-report
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
//! # Why Rust `#[allow(..)]` is **not** scanned here
//!
//! It was, and it was wrong twice over.
//!
//! *Wrong on correctness.* A line-oriented scan can only read the text after
//! the attribute's closing `)]`, so it scored as unjustified both
//! `#[allow(dead_code, reason = "…")]` — the spelling the language itself
//! sanctions, stable since Rust 1.81 — and an explaining `//` comment on the
//! line above, which is poly's own house convention for the same thing. Two
//! false-positive classes, one of them the officially correct form.
//!
//! *Wrong on judgement.* The `allow-attribute-without-reason` rule in the
//! built-in ast-grep pack already covers this, parses it properly (it accepts
//! `reason =` and a preceding comment), and ships **`severity: off`** — a hand
//! read of its 13,254 corpus findings concluded the population is overwhelmingly
//! `#[allow(non_snake_case)]` on FFI bindings and macro-generated glue, not
//! drive-by lint muting. Scanning the same population here, less accurately and
//! on by default, contradicted that finding: it made `lazy-ignore` the loudest
//! rule in poly at 10,486 corpus findings, ~95% of them these attributes.
//!
//! Rust `#[allow(..)]` therefore belongs to that pack rule, and to opting into
//! it (`extend_select = ["allow-attribute-without-reason"]`), not here.
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
}

/// One marker this rule recognizes, and how its "reason" text is read.
struct Marker {
    /// Literal substring that opens the directive.
    prefix: &'static str,
    reason: ReasonPolicy,
}

const MARKERS: &[Marker] = &[
    Marker {
        prefix: "# noqa",
        reason: ReasonPolicy::DescriptorThenReason,
    },
    Marker {
        prefix: "// eslint-disable",
        reason: ReasonPolicy::AfterDoubleDash,
    },
    Marker {
        prefix: "// oxlint-disable",
        reason: ReasonPolicy::AfterDoubleDash,
    },
    Marker {
        prefix: "// biome-ignore",
        reason: ReasonPolicy::DescriptorThenReason,
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
        let Some(rest) = find_marker(trimmed, marker.prefix) else {
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

    /// Rust `#[allow(..)]` is owned by the `allow-attribute-without-reason`
    /// pack rule, which parses it properly and ships `off`. This rule must not
    /// score it at all — not even the bare form it once flagged, since a
    /// partial reimplementation is exactly what produced the two
    /// false-positive classes below.
    #[test]
    fn defers_every_rust_allow_form_to_the_ast_grep_pack_rule() {
        for source in [
            "#[allow(dead_code)]\nfn f() {}\n",
            "#![allow(dead_code)]\n",
            "#[allow(dead_code)] // used only in benchmark builds\nfn f() {}\n",
            "// See #[allow(dead_code)] for why this is kept.\nfn f() {}\n",
        ] {
            let findings = scan(source);
            assert!(findings.is_empty(), "{source:?} produced {findings:?}");
        }
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

    /// `reason = "..."` inside the attribute is the form the Rust language
    /// itself sanctions (stable since 1.81) and the one `#[expect(..)]` users
    /// are steered toward. Reading only the text *after* `)]` cannot see it,
    /// so this rule scored the officially-correct spelling as unjustified.
    #[test]
    fn does_not_flag_rust_allow_carrying_the_official_reason_field() {
        let findings = scan("#[allow(dead_code, reason = \"kept for the C ABI\")]\nfn f() {}\n");
        assert!(findings.is_empty(), "{findings:?}");
    }

    /// A `//` comment on the line *above* the attribute is poly's own
    /// documented house convention for justifying an `#[allow(..)]`, and the
    /// form the `allow-attribute-without-reason` pack rule accepts. A
    /// same-line-only reader scores it as unjustified.
    #[test]
    fn does_not_flag_rust_allow_justified_by_the_comment_above_it() {
        let findings =
            scan("// The lint is wrong here: the field is read through FFI.\n#[allow(dead_code)]\nfn f() {}\n");
        assert!(findings.is_empty(), "{findings:?}");
    }
}
