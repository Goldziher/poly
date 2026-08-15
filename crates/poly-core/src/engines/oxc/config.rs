//! Building the `oxc_formatter` / `oxc_formatter_json` option structs from a
//! resolved [`EngineConfig`].

use oxc_formatter::JsFormatOptions;
use oxc_formatter_core::{IndentStyle, IndentWidth, LineWidth};
use oxc_formatter_json::{JsonFormatOptions, JsonVariant};

use crate::config::EngineConfig;
use crate::engine::SourceFile;
use crate::language::Language;

/// Build [`JsFormatOptions`] from a resolved [`EngineConfig`].
///
/// ## Layering order (Prettier-compatible defaults → poly overrides → user config)
///
/// | `cfg.options` key | Type | Values |
/// |---|---|---|
/// | `quote_style` | string | `"double"` (default) / `"single"` |
/// | `jsx_quote_style` | string | `"double"` (default) / `"single"` |
/// | `semicolons` | string | `"always"` (default) / `"as-needed"` |
/// | `trailing_commas` | string | `"all"` (default) / `"es5"` / `"none"` |
/// | `arrow_parentheses` | string | `"always"` (default) / `"as-needed"` |
/// | `bracket_spacing` | bool | `true` (default) |
/// | `bracket_same_line` | bool | `false` (default) |
/// | `indent_style` | string | `"space"` (default) / `"tab"` |
///
/// `line_width` and `indent_width` are always taken from `cfg.globals.line_length`
/// and `cfg.indent_width` respectively — user cannot override them here.
pub(super) fn build_js_options(cfg: &EngineConfig) -> JsFormatOptions {
    use oxc_formatter::{
        ArrowParentheses, BracketSameLine, BracketSpacing, QuoteStyle, Semicolons, TrailingCommas as JsTrailingCommas,
    };

    let line_width = u16::try_from(cfg.globals.line_length)
        .ok()
        .and_then(|w| LineWidth::try_from(w).ok())
        .unwrap_or_else(|| {
            // SAFETY: 120 is always in [LineWidth::MIN, LineWidth::MAX].
            LineWidth::try_from(120u16).expect("120 is a valid LineWidth")
        });

    let indent_width = u8::try_from(cfg.indent_width)
        .ok()
        .and_then(|w| IndentWidth::try_from(w).ok())
        .unwrap_or_default();

    // oxc dropped `impl FromStr for IndentStyle`; the other option enums below
    // keep theirs. Spelled out here rather than parsed so the accepted values
    // stay exactly what the table above documents — and match the `QuoteStyle`
    // handling immediately below.
    let indent_style = cfg
        .options
        .get("indent_style")
        .and_then(|v| v.as_str())
        .and_then(|s| match s {
            "tab" => Some(IndentStyle::Tab),
            "space" => Some(IndentStyle::Space),
            _ => None,
        })
        .unwrap_or_default();

    let quote_style = cfg
        .options
        .get("quote_style")
        .and_then(|v| v.as_str())
        .map(|s| match s {
            "single" => QuoteStyle::Single,
            _ => QuoteStyle::Double,
        })
        .unwrap_or_default();

    let jsx_quote_style = cfg
        .options
        .get("jsx_quote_style")
        .and_then(|v| v.as_str())
        .map(|s| match s {
            "single" => QuoteStyle::Single,
            _ => QuoteStyle::Double,
        })
        .unwrap_or_default();

    let semicolons = cfg
        .options
        .get("semicolons")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<Semicolons>().ok())
        .unwrap_or_default();

    let trailing_commas = cfg
        .options
        .get("trailing_commas")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<JsTrailingCommas>().ok())
        .unwrap_or_default();

    let arrow_parentheses = cfg
        .options
        .get("arrow_parentheses")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<ArrowParentheses>().ok())
        .unwrap_or_default();

    let bracket_spacing = cfg
        .options
        .get("bracket_spacing")
        .and_then(|v| v.as_bool())
        .map(BracketSpacing::from)
        .unwrap_or_default();

    let bracket_same_line = cfg
        .options
        .get("bracket_same_line")
        .and_then(|v| v.as_bool())
        .map(BracketSameLine::from)
        .unwrap_or_default();

    JsFormatOptions {
        line_width,
        indent_width,
        indent_style,
        quote_style,
        jsx_quote_style,
        semicolons,
        trailing_commas,
        arrow_parentheses,
        bracket_spacing,
        bracket_same_line,
        ..JsFormatOptions::default()
    }
}

/// Build [`JsonFormatOptions`] from a resolved [`EngineConfig`].
///
/// ## Layering order
///
/// | `cfg.options` key | Type | Values |
/// |---|---|---|
/// | `bracket_spacing` | bool | `true` (default) |
/// | `trailing_commas` | string | `"always"` (default for JSONC) / `"never"` |
///
/// `line_width` and `indent_width` are always sourced from `cfg.globals.line_length`
/// and `cfg.indent_width`. The variant (Json vs Jsonc) is derived from the file language
/// and cannot be overridden per-option.
pub(super) fn build_json_options(src: &SourceFile, cfg: &EngineConfig) -> JsonFormatOptions {
    use oxc_formatter_json::{BracketSpacing as JsonBracketSpacing, TrailingCommas as JsonTc};

    let variant = match src.language {
        Language::Jsonc => JsonVariant::Jsonc,
        _ => JsonVariant::Json,
    };

    let line_width = u16::try_from(cfg.globals.line_length)
        .ok()
        .and_then(|w| LineWidth::try_from(w).ok())
        .unwrap_or_else(|| {
            // SAFETY: 120 is always in [LineWidth::MIN, LineWidth::MAX].
            LineWidth::try_from(120u16).expect("120 is a valid LineWidth")
        });

    let indent_width = u8::try_from(cfg.indent_width)
        .ok()
        .and_then(|w| IndentWidth::try_from(w).ok())
        .unwrap_or_default();

    let bracket_spacing = cfg
        .options
        .get("bracket_spacing")
        .and_then(|v| v.as_bool())
        .map(JsonBracketSpacing::from)
        .unwrap_or_default();

    let trailing_commas = cfg
        .options
        .get("trailing_commas")
        .and_then(|v| v.as_str())
        .map(|s| match s {
            "never" => JsonTc::Never,
            _ => JsonTc::Always,
        })
        .unwrap_or_default();

    JsonFormatOptions {
        variant,
        line_width,
        indent_width,
        bracket_spacing,
        trailing_commas,
        ..JsonFormatOptions::default()
    }
}
