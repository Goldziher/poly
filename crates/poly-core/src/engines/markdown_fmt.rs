//! Markdown formatting through `oxc_formatter_markdown`, the formatter oxfmt itself uses.
//!
//! Prettier-compatible: list markers, emphasis, blank lines between blocks, table alignment and
//! escaping are normalised the way oxfmt normalises them, so a repository formatted by oxfmt is a
//! fixed point. Code inside fences and front matter is re-formatted with the matching language
//! formatter through [`super::markdown_embed`], as oxfmt does; a fence in a language poly does not
//! format in process, or one that does not parse, stays as written.
//!
//! Keys read from `[fmt.markdown.rumdl]`: `indent_width`, `indent_style`, `line_width`,
//! `line_ending`, `single_quote`, `prose_wrap` (`"preserve"` / `"always"` / `"never"`),
//! `embedded_language_formatting` (`"off"` keeps fences and front matter as written) and the
//! `embedded` option overlays.

use oxc_allocator::Allocator;
use oxc_formatter_core::{FormatOptions, FormatSession, InputKind, SessionServices};
use oxc_formatter_markdown::{MarkdownFormatOptions, ProseWrap};

use super::core_options::core_format_options;
use super::markdown_embed;
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
    let session = FormatSession::with_services(
        &allocator,
        InputKind::PhysicalFile,
        SessionServices {
            dispatcher: markdown_embed::dispatcher(cfg),
            ..SessionServices::default()
        },
    );
    let formatted = match oxc_formatter_markdown::format_with_session(&session, &src.content, build_options(cfg))
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

pub(super) fn build_options(cfg: &EngineConfig) -> MarkdownFormatOptions {
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
        run_with(content, toml::Table::new())
    }

    fn run_with(content: &str, options: toml::Table) -> String {
        let src = SourceFile {
            path: PathBuf::from("README.md"),
            language: Language::Markdown,
            content: content.into(),
        };
        let cfg = EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 2,
            options,
        };
        match format(&src, &cfg).unwrap() {
            FormatOutput::Formatted(text) => text,
            FormatOutput::Unchanged => content.to_string(),
        }
    }

    fn table(source: &str) -> toml::Table {
        source.parse().unwrap()
    }

    #[test]
    fn blank_lines_and_bullets_are_normalised_like_oxfmt() {
        assert_eq!(run("# T\n* a\n* b\n"), "# T\n\n- a\n- b\n");
    }

    #[test]
    fn fenced_javascript_is_formatted() {
        assert_eq!(
            run("```js\nconst a={b:1}\n```\n"),
            "```js\nconst a = { b: 1 };\n```\n"
        );
    }

    #[test]
    fn fenced_json_is_formatted() {
        assert_eq!(
            run("```json\n{\"a\":1,\"b\":[1,2]}\n```\n"),
            "```json\n{ \"a\": 1, \"b\": [1, 2] }\n```\n"
        );
    }

    #[test]
    fn front_matter_is_formatted_as_yaml() {
        assert_eq!(
            run("---\ntitle:   x\n---\n\n# T\n"),
            "---\ntitle: x\n---\n\n# T\n"
        );
    }

    #[test]
    fn unknown_and_unparseable_fences_are_left_alone() {
        let rust = "```rust\nfn  main( ){}\n```\n";
        assert_eq!(run(rust), rust);
        let broken = "```js\nconst = ;\n```\n";
        assert_eq!(run(broken), broken);
    }

    #[test]
    fn embedded_language_formatting_off_keeps_fences_as_written() {
        let src = "```js\nconst a={b:1}\n```\n";
        let options = table("embedded_language_formatting = \"off\"");
        assert_eq!(run_with(src, options), src);
    }

    #[test]
    fn embedded_overlay_reaches_only_its_family() {
        let options = table("[embedded.js]\nsemicolons = \"as-needed\"");
        assert_eq!(
            run_with("```js\nconst a={b:1};\n```\n", options),
            "```js\nconst a = { b: 1 }\n```\n"
        );
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
