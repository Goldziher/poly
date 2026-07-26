//! oxc format path: `oxc_formatter` for JS/TS/JSX/TSX and `oxc_formatter_json`
//! for JSON/JSONC.

use oxc_allocator::Allocator;
use oxc_span::SourceType;

use crate::config::EngineConfig;
use crate::engine::{FormatOutput, SourceFile};
use crate::language::Language;

use super::config::{build_js_options, build_json_options};

fn source_type_for(lang: &Language) -> SourceType {
    match lang {
        Language::TypeScript => SourceType::ts(),
        Language::Tsx => SourceType::tsx(),
        Language::Jsx => SourceType::jsx(),
        _ => SourceType::mjs(),
    }
}

/// Format a JS/TS/JSX/TSX file using `oxc_formatter` (Prettier-compatible).
///
/// Line width is taken from `cfg.globals.line_length` (project default: 120).
/// Additional formatter options (`quote_style`, `semicolons`, `trailing_commas`,
/// `arrow_parentheses`, `bracket_spacing`, `bracket_same_line`, `indent_style`)
/// can be set via `[fmt.<lang>.oxc]` in `poly.toml`.
pub(super) fn format_js(src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
    let allocator = Allocator::new();
    let source_type = source_type_for(&src.language);
    let options = build_js_options(cfg);

    let formatted = match oxc_formatter::format(&allocator, &src.content, source_type, options, None) {
        Err(_) => return Ok(FormatOutput::Unchanged),
        Ok(f) => f,
    };

    let printed = formatted
        .print()
        .map_err(|e| anyhow::anyhow!("oxc_formatter print error: {e}"))?;
    let mut code = printed.into_code();

    if !code.ends_with('\n') {
        code.push('\n');
    }

    if code == *src.content {
        Ok(FormatOutput::Unchanged)
    } else {
        Ok(FormatOutput::Formatted(code))
    }
}

pub(super) fn format_json(src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
    let allocator = Allocator::new();
    let options = build_json_options(src, cfg);

    let formatted = match oxc_formatter_json::format(&allocator, &src.content, options) {
        Err(_) => return Ok(FormatOutput::Unchanged),
        Ok(f) => f,
    };

    let mut code = formatted
        .print()
        .map_err(|e| anyhow::anyhow!("oxc_formatter_json print error: {e}"))?
        .into_code();

    if !code.ends_with('\n') {
        code.push('\n');
    }

    if code == *src.content {
        Ok(FormatOutput::Unchanged)
    } else {
        Ok(FormatOutput::Formatted(code))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::config::GlobalDefaults;

    fn make_src(content: &str, lang: Language) -> SourceFile {
        SourceFile {
            path: PathBuf::from("test.js"),
            language: lang,
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
    fn format_js_normalizes_spacing() {
        let src = make_src("const x={a:1,b:2};\n", Language::JavaScript);
        let cfg = default_cfg();
        let out = format_js(&src, &cfg).unwrap();
        assert!(matches!(out, FormatOutput::Formatted(_)));
    }

    #[test]
    fn format_js_returns_unchanged_for_already_formatted() {
        let src = make_src("const x = {\n  a: 1,\n  b: 2,\n};\n", Language::JavaScript);
        let cfg = default_cfg();
        let first = match format_js(&src, &cfg).unwrap() {
            FormatOutput::Formatted(s) => s,
            FormatOutput::Unchanged => src.content.to_string(),
        };
        let src2 = make_src(&first, Language::JavaScript);
        let second = format_js(&src2, &cfg).unwrap();
        assert!(
            matches!(second, FormatOutput::Unchanged),
            "second pass should be Unchanged; got: {second:?}"
        );
    }

    /// `quote_style = "single"` rewrites `"hello"` to `'hello'`.
    #[test]
    fn js_format_single_quote_style_rewrites_double_quotes() {
        let src = make_src("export const greeting = \"hello\";\n", Language::JavaScript);
        let cfg = EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 2,
            options: toml::from_str(r#"quote_style = "single""#).unwrap(),
        };
        let out = format_js(&src, &cfg).unwrap();
        match out {
            FormatOutput::Formatted(text) => {
                assert!(text.contains("'hello'"), "expected single-quoted string; got: {text:?}");
            }
            FormatOutput::Unchanged => {
                panic!("expected Formatted output with single quotes, got Unchanged");
            }
        }
    }

    /// `semicolons = "as-needed"` strips the trailing semicolons.
    #[test]
    fn js_format_semicolons_as_needed_removes_semicolons() {
        let src = make_src("export const x = 1;\nexport const y = 2;\n", Language::JavaScript);
        let cfg = EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 2,
            options: toml::from_str(r#"semicolons = "as-needed""#).unwrap(),
        };
        let out = format_js(&src, &cfg).unwrap();
        match out {
            FormatOutput::Formatted(text) => {
                assert!(!text.contains(";\n"), "expected semicolons removed; got: {text:?}");
            }
            FormatOutput::Unchanged => {}
        }
    }
}
