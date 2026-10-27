//! Layout options every oxfmt-equivalent formatter shares: line width, line ending, indentation.
//!
//! Read from the engine's `[fmt.<lang>.<engine>]` table (where `.oxfmtrc.json[c]` defaults land),
//! falling back to the `[defaults]` globals.

use oxc_formatter_core::{CoreFormatOptions, IndentStyle, IndentWidth, LineEnding, LineWidth};

use crate::config::{EngineConfig, LineEnding as ConfiguredLineEnding};

/// `line_width` from the table when set, else `[defaults] line_length`, else 120.
pub(crate) fn resolve_line_width(cfg: &EngineConfig) -> LineWidth {
    cfg.options
        .get("line_width")
        .and_then(toml::Value::as_integer)
        .map(|width| width.max(0) as usize)
        .or(Some(cfg.globals.line_length))
        .and_then(|width| u16::try_from(width).ok())
        .and_then(|width| LineWidth::try_from(width).ok())
        .unwrap_or_else(|| LineWidth::try_from(120u16).expect("120 is a valid LineWidth"))
}

/// `line_ending` (`lf` / `crlf` / `cr`) from the table, else the formatter default.
pub(crate) fn resolve_line_ending(cfg: &EngineConfig) -> LineEnding {
    match cfg.options.get("line_ending").and_then(toml::Value::as_str) {
        Some("crlf") => LineEnding::Crlf,
        Some("cr") => LineEnding::Cr,
        Some("lf") => LineEnding::Lf,
        _ => match cfg.globals.line_ending {
            ConfiguredLineEnding::Crlf => LineEnding::Crlf,
            ConfiguredLineEnding::Lf => LineEnding::Lf,
        },
    }
}

pub(crate) fn resolve_indent_style(cfg: &EngineConfig) -> IndentStyle {
    match cfg.options.get("indent_style").and_then(toml::Value::as_str) {
        Some("tab") => IndentStyle::Tab,
        _ => IndentStyle::Space,
    }
}

pub(crate) fn resolve_indent_width(cfg: &EngineConfig) -> IndentWidth {
    u8::try_from(cfg.indent_width)
        .ok()
        .and_then(|width| IndentWidth::try_from(width).ok())
        .unwrap_or_default()
}

/// All four shared options resolved for `cfg`.
pub(crate) fn core_format_options(cfg: &EngineConfig) -> CoreFormatOptions {
    CoreFormatOptions {
        indent_style: resolve_indent_style(cfg),
        indent_width: resolve_indent_width(cfg),
        line_width: resolve_line_width(cfg),
        line_ending: resolve_line_ending(cfg),
    }
}
