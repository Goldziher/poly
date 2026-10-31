//! HCL / Terraform backend — `hcl-edit` (MIT OR Apache-2.0) for parse-error linting and a
//! port of `hclwrite.Format` for formatting.
//!
//! ## Capabilities
//! - **Lint**: first syntax error from `hcl-edit`'s parser, reported as a
//!   `syntax-error` diagnostic at the 1-based line / column the parser provides.
//!   Clean files produce `vec![]`.
//! - **Format**: a token-level port of `hclwrite.Format` (what `terraform fmt` and
//!   `terragrunt hclfmt` run), so `.tf`, `.tfvars` and `.hcl` files that are already canonical
//!   stay byte-identical. It re-indents by bracket depth, normalises intra-line spacing and
//!   aligns `=` and trailing comments across consecutive lines; blank lines, comments, list
//!   and object layout are never rewritten. Syntax-invalid input is left unchanged.
//!
//! ## Config layering
//! Tool default → opinionated override (indent width 2, matching the Terraform
//! style convention) → user `[fmt.hcl.hcl]` table (`indent_width` only).

mod format;
mod lex;

use hcl_edit::parser as hcl_edit_parser;

use crate::config::EngineConfig;
use crate::engine::{
    Capabilities, Diagnostic, Engine, FormatOutput, OptionKeys, OptionTable, Severity, SourceFile, Span,
};
use crate::engine_guard::refuse_pathological_nesting;
use crate::language::Language;

/// Version string used as the Engine cache key; bump the suffix whenever formatter output changes.
const ENGINE_VERSION: &str = "hcl-edit 0.10.0 + hclwrite-format-v1";

static LANGUAGES: &[Language] = &[Language::Hcl];

/// Tier-1 HCL / Terraform backend: `hcl-edit` lint plus the `hclwrite` formatting port.
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
    /// The HCL formatter reads only `indent_width`, which every engine table accepts.
    fn option_keys(&self, _table: OptionTable) -> OptionKeys {
        OptionKeys::declared(&[])
    }

    fn version(&self) -> &str {
        ENGINE_VERSION
    }

    fn lint(&self, src: &SourceFile, _cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        refuse_pathological_nesting(&src.path, &src.content)?;
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
        refuse_pathological_nesting(&src.path, &src.content)?;
        if hcl_edit_parser::parse_body(&src.content).is_err() {
            return Ok(FormatOutput::Unchanged);
        }
        let Some(formatted) = format::format(&src.content, indent_width_from_cfg(cfg)) else {
            return Ok(FormatOutput::Unchanged);
        };
        if formatted == src.content.as_ref() || hcl_edit_parser::parse_body(&formatted).is_err() {
            return Ok(FormatOutput::Unchanged);
        }
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
    fn version_encodes_parser_crate() {
        let engine = HclEngine;
        let v = engine.version();
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
    fn format_syntax_error_is_left_unchanged() {
        let engine = HclEngine;
        let src = make_src(
            "bad.tf",
            Language::Hcl,
            "resource \"aws_instance\" \"web\" {\n  ami = \"ami-12345\"\n",
        );
        let output = engine
            .format(&src, &default_cfg())
            .expect("invalid HCL should be skipped without failing the repository run");

        assert!(matches!(output, FormatOutput::Unchanged));
    }

    #[test]
    fn format_syntax_error_with_comment_like_text_is_left_unchanged() {
        let engine = HclEngine;
        let src = make_src(
            "bad.tf",
            Language::Hcl,
            r####"provisioner "local-exec" {
  command = "curl https://${var.channel}/version.txt | sed -n 's/VERSION=\(.*\)$/{"version": "\1"}/p'"
}
"####,
        );
        let output = engine
            .format(&src, &default_cfg())
            .expect("invalid HCL containing comment-like text should be skipped");

        assert!(matches!(output, FormatOutput::Unchanged));
    }

    #[test]
    fn format_does_not_emit_invalid_escaped_string() {
        let engine = HclEngine;
        let src = make_src(
            "main.tf",
            Language::Hcl,
            r####"data "external" "version" {
  count   = "${var.release_version == "latest" ? 1 : 0}"
  program = ["sh", "-c", "curl https://${var.release_channel}/version.txt | sed -n 's/VERSION=\\(.*\\)$/{\"version\": \"\\1\"}/p'"]
}
"####,
        );
        let output = engine.format(&src, &default_cfg()).unwrap();

        assert!(matches!(output, FormatOutput::Unchanged));
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
    fn format_commented_file_keeps_comments() {
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

    fn formatted(content: &str) -> String {
        let src = make_src("main.tf", Language::Hcl, content);
        match HclEngine.format(&src, &default_cfg()).unwrap() {
            FormatOutput::Unchanged => content.to_string(),
            FormatOutput::Formatted(out) => out,
        }
    }

    #[test]
    fn aligns_equals_across_consecutive_attributes() {
        assert_eq!(
            formatted("module \"m\" {\n  source=\"x\"\n  name   =   \"y\"\n  a_long_key = 1\n}\n"),
            "module \"m\" {\n  source     = \"x\"\n  name       = \"y\"\n  a_long_key = 1\n}\n"
        );
    }

    #[test]
    fn blank_line_breaks_alignment_group_and_is_kept() {
        let canonical = "locals {\n  a = 1\n\n  long_name = 2\n}\n";
        assert_eq!(formatted(canonical), canonical);
    }

    #[test]
    fn multiline_lists_and_objects_are_not_collapsed() {
        let canonical = "locals {\n  ports = [\n    80,\n    443,\n  ]\n  tags = {\n    a = \"b\"\n  }\n}\n";
        assert_eq!(formatted(canonical), canonical);
    }

    #[test]
    fn trailing_comments_are_aligned() {
        assert_eq!(
            formatted("locals {\n  a = 1 # one\n  long_name = 2 # two\n}\n"),
            "locals {\n  a         = 1 # one\n  long_name = 2 # two\n}\n"
        );
    }

    #[test]
    fn heredoc_bodies_are_untouched() {
        let canonical = "locals {\n  script = <<-EOT\n      echo   \"${var.x}\"\n  EOT\n}\n";
        assert_eq!(formatted(canonical), canonical);
    }

    #[test]
    fn interpolation_and_operator_spacing_is_normalised() {
        assert_eq!(
            formatted("locals {\n  a = \"${ var.x }\"\n  b = var.a?1:-2\n  c = [for k in var.l: upper(k)]\n}\n"),
            "locals {\n  a = \"${var.x}\"\n  b = var.a ? 1 : -2\n  c = [for k in var.l : upper(k)]\n}\n"
        );
    }

    #[test]
    fn dedents_misindented_blocks() {
        assert_eq!(
            formatted("resource \"a\" \"b\"{\n        x = 1\n    }\n"),
            "resource \"a\" \"b\" {\n  x = 1\n}\n"
        );
    }
}
