//! Formatter options read from `[fmt.python.ruff]`.
//!
//! The keys keep ruff's `[format]` vocabulary with underscores: `line_length`,
//! `indent_width`, `indent_style`, `quote_style`, `skip_magic_trailing_comma`,
//! `line_ending`, `docstring_code_format` and `docstring_code_line_length` (an
//! integer, or `"dynamic"`). Poly's defaults differ from ruff's where it says so
//! in [`super`]: width 120 and docstring code formatting on.

use std::path::Path;

use ruff_formatter::printer::LineEnding;
use ruff_formatter::{IndentStyle, IndentWidth, LineWidth};
use ruff_python_formatter::{
    DocstringCode, DocstringCodeLineWidth, MagicTrailingComma, PreviewMode, PyFormatOptions, QuoteStyle,
};

use crate::config::EngineConfig;
use crate::engine::{OptionType, SourceFile};

/// The keys `[fmt.python.ruff]` reads, with the type each is read as.
///
/// A named const rather than an inline literal because
/// `docstring_code_line_length` takes a `const fn` union, and rustc does not
/// promote a temporary array holding one to `'static`.
pub(super) const OPTION_KEYS: &[(&str, OptionType)] = &[
    ("line_length", OptionType::INTEGER),
    ("docstring_code_format", OptionType::BOOLEAN),
    // `"dynamic"` is the documented alternative to a fixed width.
    ("docstring_code_line_length", OptionType::INTEGER.or(OptionType::STRING)),
    ("indent_style", OptionType::STRING),
    ("quote_style", OptionType::STRING),
    ("skip_magic_trailing_comma", OptionType::BOOLEAN),
    ("line_ending", OptionType::STRING),
    ("preview", OptionType::BOOLEAN),
    ("exclude", OptionType::ARRAY),
];

fn width(value: usize) -> LineWidth {
    u16::try_from(value)
        .ok()
        .and_then(|w| LineWidth::try_from(w).ok())
        .unwrap_or_else(|| LineWidth::try_from(120_u16).expect("120 is a valid line width"))
}

fn indent_style(cfg: &EngineConfig) -> Option<IndentStyle> {
    match cfg.options.get("indent_style").and_then(toml::Value::as_str)? {
        "space" => Some(IndentStyle::Space),
        "tab" => Some(IndentStyle::Tab),
        other => {
            tracing::warn!(
                key = "indent_style",
                value = other,
                engine = "ruff",
                "expected `space` or `tab`"
            );
            None
        }
    }
}

fn quote_style(cfg: &EngineConfig) -> Option<QuoteStyle> {
    match cfg.options.get("quote_style").and_then(toml::Value::as_str)? {
        "double" => Some(QuoteStyle::Double),
        "single" => Some(QuoteStyle::Single),
        "preserve" => Some(QuoteStyle::Preserve),
        other => {
            tracing::warn!(
                key = "quote_style",
                value = other,
                engine = "ruff",
                "expected `double`, `single` or `preserve`"
            );
            None
        }
    }
}

/// Whether `exclude` keeps the formatter away from `path`.
///
/// The engine is handed no config root, so each pattern matches at any depth, the
/// way a gitignore pattern without a leading slash does; a pattern also covers
/// everything below a directory it names.
pub(super) fn excluded(path: &Path, cfg: &EngineConfig) -> bool {
    let Some(patterns) = cfg.options.get("exclude").and_then(toml::Value::as_array) else {
        return false;
    };
    let mut builder = globset::GlobSetBuilder::new();
    for pattern in patterns.iter().filter_map(toml::Value::as_str) {
        let pattern = pattern
            .strip_prefix("./")
            .unwrap_or(pattern)
            .trim_start_matches('/')
            .trim_end_matches('/');
        for glob in [format!("**/{pattern}"), format!("**/{pattern}/**")] {
            if let Ok(glob) = globset::Glob::new(&glob) {
                builder.add(glob);
            }
        }
    }
    builder.build().is_ok_and(|set| set.is_match(path))
}

/// The line ending a `line_ending` value selects for `content`.
///
/// `auto` follows the first line break in the file, so a CRLF file stays CRLF;
/// `native` follows the platform. A file with no line break keeps `\n`.
fn line_ending(cfg: &EngineConfig, content: &str) -> Option<LineEnding> {
    match cfg.options.get("line_ending").and_then(toml::Value::as_str)? {
        "lf" => Some(LineEnding::LineFeed),
        "cr-lf" => Some(LineEnding::CarriageReturnLineFeed),
        "native" => Some(if cfg!(windows) {
            LineEnding::CarriageReturnLineFeed
        } else {
            LineEnding::LineFeed
        }),
        "auto" => Some(detect_line_ending(content)),
        other => {
            tracing::warn!(
                key = "line_ending",
                value = other,
                engine = "ruff",
                "expected `auto`, `lf`, `cr-lf` or `native`"
            );
            None
        }
    }
}

fn detect_line_ending(content: &str) -> LineEnding {
    let bytes = content.as_bytes();
    match bytes.iter().position(|b| matches!(b, b'\n' | b'\r')) {
        Some(at) if bytes[at] == b'\r' && bytes.get(at + 1) == Some(&b'\n') => LineEnding::CarriageReturnLineFeed,
        Some(at) if bytes[at] == b'\r' => LineEnding::CarriageReturn,
        _ => LineEnding::LineFeed,
    }
}

/// Build [`PyFormatOptions`] from the `[fmt.python.ruff]` table layered over the
/// opinionated defaults (line width from `[defaults] line_length`, docstring code
/// formatting on at the same width).
pub(super) fn build_options(src: &SourceFile, cfg: &EngineConfig) -> PyFormatOptions {
    let line_width = width(
        cfg.options
            .get("line_length")
            .and_then(toml::Value::as_integer)
            .and_then(|v| usize::try_from(v).ok())
            .unwrap_or(cfg.globals.line_length),
    );

    let docstring_code = match cfg.options.get("docstring_code_format").and_then(toml::Value::as_bool) {
        Some(false) => DocstringCode::Disabled,
        _ => DocstringCode::Enabled,
    };

    let docstring_width = match cfg.options.get("docstring_code_line_length") {
        Some(toml::Value::Integer(v)) => {
            DocstringCodeLineWidth::Fixed(width(usize::try_from(*v).unwrap_or(cfg.globals.line_length)))
        }
        Some(toml::Value::String(s)) if s.eq_ignore_ascii_case("dynamic") => DocstringCodeLineWidth::Dynamic,
        _ => DocstringCodeLineWidth::Fixed(line_width),
    };

    let mut options = PyFormatOptions::from_extension(&src.path)
        .with_line_width(line_width)
        .with_docstring_code(docstring_code)
        .with_docstring_code_line_width(docstring_width);

    if let Ok(indent_width) = u8::try_from(cfg.indent_width)
        && let Ok(indent_width) = IndentWidth::try_from(indent_width)
    {
        options = options.with_indent_width(indent_width);
    }
    if let Some(style) = indent_style(cfg) {
        options = options.with_indent_style(style);
    }
    if let Some(style) = quote_style(cfg) {
        options = options.with_quote_style(style);
    }
    if cfg
        .options
        .get("skip_magic_trailing_comma")
        .and_then(toml::Value::as_bool)
        == Some(true)
    {
        options = options.with_magic_trailing_comma(MagicTrailingComma::Ignore);
    }
    if let Some(ending) = line_ending(cfg, &src.content) {
        options = options.with_line_ending(ending);
    }
    if cfg.options.get("preview").and_then(toml::Value::as_bool) == Some(true) {
        options = options.with_preview(PreviewMode::Enabled);
    }
    options
}
