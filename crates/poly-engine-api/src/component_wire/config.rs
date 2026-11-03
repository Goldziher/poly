//! The wire form of an [`EngineConfig`] carried by `Request::RegisterConfig`.
//!
//! A table with these keys:
//!
//! - `indent_width` (integer, required): the file language's indent width.
//! - `options` (table, optional): the engine's `[<kind>.<lang>.<engine>]` options.
//! - the global defaults, each optional and defaulting to poly's own: `line_length` (integer),
//!   `line_ending` (`"lf"` or `"crlf"`), `final_newline` and `trim_trailing_whitespace` (booleans).
//!
//! Unknown keys are a protocol error, so a host and component that disagree on the shape fail loudly.

use poly_component_protocol::{WireError, WireErrorKind, WireValue, to_toml};

use crate::config::{EngineConfig, GlobalDefaults, LineEnding};

/// Rebuild an [`EngineConfig`] from its wire table.
pub fn engine_config_from_wire(value: &WireValue) -> Result<EngineConfig, WireError> {
    let WireValue::Table(pairs) = value else {
        return Err(protocol("an engine config must be a table"));
    };
    let mut globals = GlobalDefaults::default();
    let mut indent_width = None;
    let mut options = toml::Table::new();
    for (key, item) in pairs {
        match (key.as_str(), item) {
            ("indent_width", WireValue::Integer(n)) => indent_width = Some(unsigned(key, *n)?),
            ("line_length", WireValue::Integer(n)) => globals.line_length = unsigned(key, *n)?,
            ("line_ending", WireValue::String(s)) => {
                globals.line_ending = match s.as_str() {
                    "lf" => LineEnding::Lf,
                    "crlf" => LineEnding::Crlf,
                    other => return Err(protocol(format!("unknown line_ending `{other}`"))),
                }
            }
            ("final_newline", WireValue::Boolean(b)) => globals.final_newline = *b,
            ("trim_trailing_whitespace", WireValue::Boolean(b)) => globals.trim_trailing_whitespace = *b,
            ("options", table @ WireValue::Table(_)) => {
                options = match to_toml(table).map_err(|e| protocol(format!("invalid options: {e}")))? {
                    toml::Value::Table(table) => table,
                    _ => unreachable!("a wire table converts to a toml table"),
                };
            }
            (
                known @ ("indent_width"
                | "line_length"
                | "line_ending"
                | "final_newline"
                | "trim_trailing_whitespace"
                | "options"),
                _,
            ) => return Err(protocol(format!("engine config key `{known}` has the wrong type"))),
            (other, _) => return Err(protocol(format!("unknown engine config key `{other}`"))),
        }
    }
    let indent_width = indent_width.ok_or_else(|| protocol("engine config is missing `indent_width`"))?;
    Ok(EngineConfig {
        globals,
        indent_width,
        options,
    })
}

/// The wire table for `config`; the inverse of [`engine_config_from_wire`], for hosts.
pub fn engine_config_to_wire(config: &EngineConfig) -> WireValue {
    let globals = &config.globals;
    WireValue::Table(vec![
        ("indent_width".into(), WireValue::Integer(config.indent_width as i64)),
        ("line_length".into(), WireValue::Integer(globals.line_length as i64)),
        (
            "line_ending".into(),
            WireValue::String(
                match globals.line_ending {
                    LineEnding::Lf => "lf",
                    LineEnding::Crlf => "crlf",
                }
                .into(),
            ),
        ),
        ("final_newline".into(), WireValue::Boolean(globals.final_newline)),
        (
            "trim_trailing_whitespace".into(),
            WireValue::Boolean(globals.trim_trailing_whitespace),
        ),
        ("options".into(), WireValue::from(&config.options)),
    ])
}

fn unsigned(key: &str, n: i64) -> Result<usize, WireError> {
    usize::try_from(n).map_err(|_| protocol(format!("engine config key `{key}` must not be negative")))
}

fn protocol(message: impl Into<String>) -> WireError {
    WireError {
        kind: WireErrorKind::Protocol,
        message: message.into(),
        chain: Vec::new(),
    }
}
