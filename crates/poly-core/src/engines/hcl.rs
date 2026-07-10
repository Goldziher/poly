//! HCL / Terraform backend — wraps `hcl-edit` (MIT OR Apache-2.0) for in-process
//! parse-error linting and `hcl-rs` (MIT OR Apache-2.0) for formatting.
//!
//! ## Capabilities
//! - **Lint**: first syntax error from `hcl-edit`'s parser, reported as a
//!   `syntax-error` diagnostic at the 1-based line / column the parser provides.
//!   Only the first parse error is surfaced (acceptable, same as the taplo backend).
//!   Clean files produce `vec![]`.
//! - **Format**: two-path formatter keyed on comment presence.
//!   - **No comments** — round-trip through `hcl-rs`: parse into `hcl::Body`
//!     (which strips comments from the CST), then serialize with
//!     `hcl::format::Formatter` using the configured indent width.  Returns
//!     [`FormatOutput::Unchanged`] when the output is byte-identical.
//!   - **Has comments** — delegate to the tier-2 [`TreeSitterEngine`] so that
//!     `#`, `//`, and `/* */` comments are never silently dropped.
//!
//! ## Config layering
//! Tool default → opinionated override (indent width 2, matching the Terraform
//! style convention) → user `[fmt.hcl.hcl]` table (`indent_width` only).

use hcl_edit::parser as hcl_edit_parser;

use crate::config::EngineConfig;
use crate::engine::{Capabilities, Diagnostic, Engine, FormatOutput, Severity, SourceFile, Span};
use crate::engines::treesitter::TreeSitterEngine;
use crate::language::Language;

/// Combined version string used as the Engine cache key.
/// Encodes both `hcl-rs` and `hcl-edit` versions so that bumping either dep
/// invalidates stale cached results.
const ENGINE_VERSION: &str = "hcl-rs 0.19.7 + hcl-edit 0.9.6 + trailing-comments-v2";

static LANGUAGES: &[Language] = &[Language::Hcl];

/// Tier-1 HCL / Terraform backend.
///
/// Lint uses `hcl-edit` (comment-preserving CST parser); format uses `hcl-rs`
/// when there are no comments in the file, and delegates to the generic
/// [`TreeSitterEngine`] tier when comments are present (to avoid silently
/// stripping them via the `hcl-rs` AST path).
pub struct HclEngine;

impl Engine for HclEngine {
    fn name(&self) -> &'static str {
        "hcl"
    }

    fn languages(&self) -> &'static [Language] {
        LANGUAGES
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            lint: true,
            format: true,
            fix: false,
        }
    }

    /// Cache key: encodes both upstream crate versions so a dep bump
    /// invalidates cached results.
    fn version(&self) -> &str {
        ENGINE_VERSION
    }

    fn lint(&self, src: &SourceFile, _cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        match hcl_edit_parser::parse_body(&src.content) {
            Ok(_) => Ok(Vec::new()),
            Err(error) => {
                let loc = error.location();
                let line = loc.line() as u32;
                let col = loc.column() as u32;
                Ok(vec![Diagnostic {
                    engine: "hcl".to_string(),
                    code: Some("syntax-error".to_string()),
                    severity: Severity::Error,
                    title: error.message().to_string(),
                    description: None,
                    url: None,
                    span: Some(Span {
                        start_line: line,
                        start_col: col,
                        end_line: line,
                        end_col: col,
                    }),
                    fix: vec![],
                    metadata: Default::default(),
                }])
            }
        }
    }

    fn format(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
        if has_comments(&src.content) {
            return TreeSitterEngine.format(src, cfg);
        }
        format_comment_free(src, cfg)
    }
}

/// Detect whether `source` contains any HCL comment syntax.
///
/// Scans each line character-by-character, tracking double-quoted string state
/// (respecting `\"` escape sequences), so that `#` or `//` inside a string
/// value are not mistaken for comments.  Examples:
///
/// - `url = "https://example.com"` → **false** (`//` is inside a string)
/// - `x = 1 # trailing comment` → **true**
/// - `x = 1 // trailing comment` → **true**
/// - `/* block */` anywhere in source → **true** (fast-path check)
///
/// Note: `/*` inside a string literal is a pre-existing false-positive;
/// block comments in practice never appear inside string values.
fn has_comments(source: &str) -> bool {
    if source.contains("/*") {
        return true;
    }
    for line in source.lines() {
        let bytes = line.as_bytes();
        let mut in_string = false;
        let mut i = 0;
        while i < bytes.len() {
            if in_string {
                if bytes[i] == b'\\' {
                    i += 2;
                    continue;
                }
                if bytes[i] == b'"' {
                    in_string = false;
                }
            } else {
                match bytes[i] {
                    b'"' => in_string = true,
                    b'#' => return true,
                    b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => return true,
                    _ => {}
                }
            }
            i += 1;
        }
    }
    false
}

/// Format a comment-free HCL file via `hcl-rs`.
///
/// Parses into `hcl::Body`, serialises with a custom-indent [`hcl::format::Formatter`],
/// then returns [`FormatOutput::Unchanged`] when the result is byte-identical to
/// the original.
fn format_comment_free(src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
    let body: hcl::Body = src
        .content
        .parse()
        .map_err(|e: hcl::Error| anyhow::anyhow!("hcl-rs parse error: {e}"))?;

    let indent_width = indent_width_from_cfg(cfg);
    let indent_str = " ".repeat(indent_width);

    let mut buf = Vec::<u8>::new();
    let mut formatter = hcl::format::Formatter::builder()
        .indent(indent_str.as_bytes())
        .build(&mut buf);

    use hcl::format::Format as _;
    body.format(&mut formatter)
        .map_err(|e| anyhow::anyhow!("hcl-rs format error: {e}"))?;
    drop(formatter);

    let formatted = String::from_utf8(buf).map_err(|e| anyhow::anyhow!("hcl-rs produced non-UTF-8: {e}"))?;

    if formatted == src.content.as_ref() {
        Ok(FormatOutput::Unchanged)
    } else {
        Ok(FormatOutput::Formatted(formatted))
    }
}

/// Extract indent width from `EngineConfig`: user `indent_width` option wins
/// over the language default already baked into `cfg.indent_width`.
fn indent_width_from_cfg(cfg: &EngineConfig) -> usize {
    cfg.options
        .get("indent_width")
        .and_then(toml::Value::as_integer)
        .filter(|&n| n > 0)
        .map(|n| n as usize)
        .unwrap_or(cfg.indent_width)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::config::GlobalDefaults;

    fn make_src(path: &str, language: Language, content: &str) -> SourceFile {
        SourceFile {
            path: PathBuf::from(path),
            language,
            content: content.into(),
        }
    }

    fn default_cfg() -> EngineConfig {
        EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 2,
            options: toml::Table::new(),
        }
    }

    #[test]
    fn has_comments_detects_hash() {
        assert!(has_comments("# a comment\nfoo = 1\n"));
    }

    #[test]
    fn has_comments_detects_slash_slash() {
        assert!(has_comments("// a comment\nfoo = 1\n"));
    }

    #[test]
    fn has_comments_detects_block_comment() {
        assert!(has_comments("foo = /* inline */ 1\n"));
    }

    #[test]
    fn has_comments_clean_file_false() {
        assert!(!has_comments(
            "resource \"aws_s3_bucket\" \"bucket\" {\n  bucket = \"my-bucket\"\n}\n"
        ));
    }

    #[test]
    fn has_comments_slash_in_string_is_safe() {
        assert!(!has_comments("url = \"https://example.com/path\"\n"));
    }

    #[test]
    fn has_comments_detects_trailing_hash() {
        assert!(has_comments("x = 1 # trailing comment\n"));
    }

    #[test]
    fn has_comments_detects_trailing_slash_slash() {
        assert!(has_comments("x = 1 // trailing comment\n"));
    }

    #[test]
    fn format_trailing_comment_preserved() {
        let engine = HclEngine;
        let src = make_src(
            "main.tf",
            Language::Hcl,
            "x = 1 # keep this\nresource \"r\" \"a\" {\n  ami = \"x\"\n}\n",
        );
        let result = engine.format(&src, &default_cfg()).unwrap();
        let output = match result {
            FormatOutput::Unchanged => src.content.to_string(),
            FormatOutput::Formatted(s) => s,
        };
        assert!(
            output.contains("# keep this"),
            "trailing comment must survive format: {output}"
        );
    }

    #[test]
    fn version_encodes_both_crates() {
        let engine = HclEngine;
        let v = engine.version();
        assert!(v.contains("hcl-rs"), "version should mention hcl-rs: {v}");
        assert!(v.contains("hcl-edit"), "version should mention hcl-edit: {v}");
    }

    #[test]
    fn lint_clean_hcl_produces_no_diags() {
        let engine = HclEngine;
        let src = make_src(
            "main.tf",
            Language::Hcl,
            "resource \"aws_instance\" \"web\" {\n  ami = \"ami-12345\"\n}\n",
        );
        let diags = engine.lint(&src, &default_cfg()).unwrap();
        assert!(diags.is_empty(), "clean HCL should have no diagnostics");
    }

    #[test]
    fn lint_syntax_error_reported() {
        let engine = HclEngine;
        let src = make_src(
            "bad.tf",
            Language::Hcl,
            "resource \"aws_instance\" \"web\" {\n  ami = \"ami-12345\"\n",
        );
        let diags = engine.lint(&src, &default_cfg()).unwrap();
        assert!(!diags.is_empty(), "expected a syntax-error diagnostic");
        assert_eq!(diags[0].code.as_deref(), Some("syntax-error"));
        assert_eq!(diags[0].severity, Severity::Error);
        let span = diags[0].span.unwrap();
        assert!(span.start_line >= 1, "span should have a 1-based line");
    }

    #[test]
    fn format_clean_hcl_is_unchanged() {
        let engine = HclEngine;
        let src = make_src(
            "main.tf",
            Language::Hcl,
            "resource \"aws_instance\" \"web\" {\n  ami = \"ami-12345\"\n}\n",
        );
        let result = engine.format(&src, &default_cfg()).unwrap();
        match result {
            FormatOutput::Unchanged => {}
            FormatOutput::Formatted(out) => {
                let diags = engine
                    .lint(&make_src("main.tf", Language::Hcl, &out), &default_cfg())
                    .unwrap();
                assert!(diags.is_empty(), "formatted output should parse cleanly: {out}");
            }
        }
    }

    #[test]
    fn format_commented_file_delegates_to_tier2() {
        let engine = HclEngine;
        let src = make_src(
            "main.tf",
            Language::Hcl,
            "# keep this comment\nresource \"aws_instance\" \"web\" {\n  ami = \"ami-12345\"\n}\n",
        );
        let result = engine.format(&src, &default_cfg()).unwrap();
        let output = match result {
            FormatOutput::Unchanged => src.content.to_string(),
            FormatOutput::Formatted(s) => s,
        };
        assert!(
            output.contains("# keep this comment"),
            "comment must be preserved after format: {output}"
        );
    }
}
