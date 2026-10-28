//! Embedded-language dispatch for Markdown: fenced code blocks and front matter.
//!
//! `oxc_formatter_markdown` hands every fence and the front matter to a [`FormatDispatcher`]
//! installed on its session; with none, they stay as written. This module is poly's dispatcher,
//! the counterpart of oxfmt's: the languages poly formats in process (JavaScript, TypeScript,
//! JSON, YAML, CSS, SCSS, Less, TOML, nested Markdown) are formatted, and any other fence, or one
//! that fails to parse, is kept verbatim.
//!
//! Children inherit the host's table, as in oxfmt. The family options oxfmt reads from one shared
//! config (`semi`, `singleQuote` for JavaScript, `trailingComma` for JSON, ...) arrive in the
//! `embedded` sub-table, one entry per family (`js`, `json`, `yaml`, `css`, `toml`), which is laid
//! over the host table for that child. `embedded_language_formatting = "off"` removes the
//! dispatcher altogether.

use std::sync::Arc;

use oxc_formatter::JsEmbeddedIn;
use oxc_formatter_core::{DispatchRequest, DispatchResponse, FormatDispatcher, FormatSession};
use oxc_formatter_css::CssVariant;
use oxc_formatter_json::JsonVariant;
use oxc_formatter_markdown::XxxInMarkdownCodeBlock;
use oxc_span::SourceType;

use crate::config::EngineConfig;

/// Sub-table of `[fmt.markdown.rumdl]` holding per-family option overlays for embedded code.
pub(super) const EMBEDDED_KEY: &str = "embedded";
/// `"off"` keeps fenced code and front matter exactly as written.
pub(super) const EMBEDDED_FORMATTING_KEY: &str = "embedded_language_formatting";

enum Target {
    Js(SourceType),
    Css(CssVariant),
    Json(JsonVariant),
    Yaml,
    Toml,
    Markdown,
}

fn route(language: &str) -> Option<Target> {
    let script = |extension: &str| SourceType::from_extension(extension).ok().map(Target::Js);
    match language {
        "javascript" | "js" => script("js"),
        "jsx" => script("jsx"),
        "typescript" | "ts" | "angular-ts" => script("ts"),
        "tsx" => script("tsx"),
        "mjs" | "cjs" | "mts" | "cts" => script(language),
        "json" => Some(Target::Json(JsonVariant::Json)),
        "jsonc" => Some(Target::Json(JsonVariant::Jsonc)),
        "json5" => Some(Target::Json(JsonVariant::Json5)),
        "css" | "postcss" => Some(Target::Css(CssVariant::Css)),
        "scss" => Some(Target::Css(CssVariant::Scss)),
        "less" => Some(Target::Css(CssVariant::Less)),
        "yaml" | "yml" => Some(Target::Yaml),
        "toml" => Some(Target::Toml),
        "markdown" | "md" => Some(Target::Markdown),
        _ => None,
    }
}

/// The host table with the `embedded.<family>` overlay applied, as one child formatter sees it.
fn child_config(host: &EngineConfig, family: &str) -> EngineConfig {
    let mut options = host.options.clone();
    options.remove(EMBEDDED_KEY);
    let mut indent_width = host.indent_width;
    let overlay = host
        .options
        .get(EMBEDDED_KEY)
        .and_then(toml::Value::as_table)
        .and_then(|families| families.get(family))
        .and_then(toml::Value::as_table);
    if let Some(overlay) = overlay {
        for (key, value) in overlay {
            options.insert(key.clone(), value.clone());
        }
        if let Some(width) = overlay
            .get("indent_width")
            .and_then(toml::Value::as_integer)
            .and_then(|width| usize::try_from(width).ok())
        {
            indent_width = width;
        }
    }
    EngineConfig {
        globals: host.globals.clone(),
        indent_width,
        options,
    }
}

struct ChildConfigs {
    js: EngineConfig,
    json: EngineConfig,
    yaml: EngineConfig,
    css: EngineConfig,
    toml: EngineConfig,
    markdown: EngineConfig,
}

impl ChildConfigs {
    fn new(host: &EngineConfig) -> Self {
        Self {
            js: child_config(host, "js"),
            json: child_config(host, "json"),
            yaml: child_config(host, "yaml"),
            css: child_config(host, "css"),
            toml: child_config(host, "toml"),
            markdown: child_config(host, "markdown"),
        }
    }
}

/// The dispatcher for a Markdown file formatted under `host`, or `None` when embedded
/// formatting is switched off.
pub(super) fn dispatcher(host: &EngineConfig) -> Option<FormatDispatcher> {
    if host.options.get(EMBEDDED_FORMATTING_KEY).and_then(toml::Value::as_str) == Some("off") {
        return None;
    }
    Some(build(Arc::new(ChildConfigs::new(host))))
}

fn build(configs: Arc<ChildConfigs>) -> FormatDispatcher {
    Arc::new(move |session: &FormatSession<'_>, request: DispatchRequest<'_>| {
        let Some(target) = route(request.language) else {
            return Ok(DispatchResponse::PreserveOriginal);
        };
        let text = request.text;
        let embedded = match target {
            Target::Js(source_type) => {
                let embedded_in = request
                    .parent_context_as::<XxxInMarkdownCodeBlock>()
                    .map(|_| JsEmbeddedIn::MarkdownCodeBlock);
                oxc_formatter::format_to_ir(
                    session,
                    text,
                    source_type,
                    super::oxc::embedded_js_options(&configs.js),
                    embedded_in,
                )
            }
            Target::Css(variant) => oxc_formatter_css::format_to_ir(
                session,
                text,
                super::malva::oxc_css_options(&configs.css, variant),
                false,
            ),
            Target::Json(variant) => oxc_formatter_json::format_to_ir(
                session,
                text,
                super::oxc::embedded_json_options(&configs.json, variant),
            ),
            Target::Yaml => {
                oxc_formatter_yaml::format_to_ir(session, text, super::yaml::build_format_options(&configs.yaml))
            }
            Target::Toml => {
                oxc_formatter_toml::format_to_ir(session, text, super::taplo::build_oxc_options(&configs.toml))
            }
            Target::Markdown => {
                let in_js_template = request
                    .parent_context_as::<XxxInMarkdownCodeBlock>()
                    .is_some_and(|context| context.in_js_template);
                oxc_formatter_markdown::format_to_ir(
                    session,
                    text,
                    super::markdown_fmt::build_options(&configs.markdown),
                    in_js_template,
                )
            }
        };
        Ok(embedded.map_or(DispatchResponse::PreserveOriginal, DispatchResponse::Formatted))
    })
}
