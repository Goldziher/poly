//! Markdown formatting through `oxc_formatter_markdown`, the formatter oxfmt itself uses.
//!
//! Prettier-compatible: list markers, emphasis, blank lines between blocks, table alignment and
//! escaping are normalised the way oxfmt normalises them, so a repository formatted by oxfmt is a
//! fixed point. Code inside fences and front matter is left as written (oxfmt re-formats it with
//! the matching language formatter; that dispatch lives in its binary and is not available here).
//!
//! Keys read from `[fmt.markdown.rumdl]`: `indent_width`, `indent_style`, `line_width`,
//! `line_ending`, `single_quote` and `prose_wrap` (`"preserve"` / `"always"` / `"never"`).

use oxc_allocator::Allocator;
use oxc_formatter_core::FormatOptions;
use oxc_formatter_markdown::{MarkdownFormatOptions, ProseWrap};

use super::core_options::core_format_options;
use crate::config::EngineConfig;
use crate::engine::{FormatOutput, SourceFile};

/// Keys that belong to rumdl's own rule-based formatter; setting one keeps that formatter.
pub(super) const RUMDL_FORMAT_KEYS: &[&str] = &[
    "line_length",
    "enable",
    "extend_enable",
    "disable",
    "select",
    "extend_select",
    "ignore",
    "rules",
];

pub(super) fn uses_rumdl_formatter(cfg: &EngineConfig) -> bool {
    RUMDL_FORMAT_KEYS.iter().any(|key| cfg.options.contains_key(*key))
}

pub(super) fn format(src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
    let allocator = Allocator::new();
    let formatted = match oxc_formatter_markdown::format(&allocator, &src.content, build_options(cfg))
        .map_err(|_| ())
        .and_then(|formatted| formatted.print().map_err(|_| ()))
    {
        Ok(printed) => printed.into_code(),
        Err(()) => return Ok(FormatOutput::Unchanged),
    };
    if formatted == *src.content {
        Ok(FormatOutput::Unchanged)
    } else {
        Ok(FormatOutput::Formatted(formatted))
    }
}

fn build_options(cfg: &EngineConfig) -> MarkdownFormatOptions {
    let mut options = MarkdownFormatOptions::default();
    options.apply_core(core_format_options(cfg));
    if let Some(single) = cfg.options.get("single_quote").and_then(toml::Value::as_bool) {
        options.single_quote = single.into();
    }
    options.prose_wrap = match cfg.options.get("prose_wrap").and_then(toml::Value::as_str) {
        Some("always") => ProseWrap::Always,
        Some("never") => ProseWrap::Never,
        _ => ProseWrap::Preserve,
    };
    options
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::config::GlobalDefaults;
    use crate::language::Language;

    fn run(content: &str) -> String {
        let src = SourceFile {
            path: PathBuf::from("README.md"),
            language: Language::Markdown,
            content: content.into(),
        };
        let cfg = EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 2,
            options: toml::Table::new(),
        };
        match format(&src, &cfg).unwrap() {
            FormatOutput::Formatted(text) => text,
            FormatOutput::Unchanged => content.to_string(),
        }
    }

    #[test]
    fn blank_lines_and_bullets_are_normalised_like_oxfmt() {
        assert_eq!(run("# T\n* a\n* b\n"), "# T\n\n- a\n- b\n");
    }

    #[test]
    fn fenced_code_is_left_alone() {
        let src = "```js\nconst a={b:1}\n```\n";
        assert_eq!(run(src), src);
    }

    #[test]
    fn rumdl_keys_select_the_rule_based_formatter() {
        let mut options = toml::Table::new();
        options.insert("disable".to_string(), toml::Value::Array(vec![]));
        let cfg = EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 2,
            options,
        };
        assert!(uses_rumdl_formatter(&cfg));
    }
}
