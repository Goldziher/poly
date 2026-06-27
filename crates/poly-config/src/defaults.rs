//! Opinionated global defaults (`[defaults]`), shared by lint and format.

use serde::Deserialize;

/// Line-ending style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    /// Unix `\n`.
    Lf,
    /// Windows `\r\n`.
    Crlf,
}

impl LineEnding {
    /// The line-ending byte sequence (`"\n"` or `"\r\n"`).
    pub fn as_str(self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::Crlf => "\r\n",
        }
    }
}

/// Opinionated global defaults applied wherever a tool exposes the setting.
#[derive(Debug, Clone)]
pub struct GlobalDefaults {
    /// Target maximum line length (applied where a tool exposes it).
    pub line_length: usize,
    /// Line-ending style to enforce.
    pub line_ending: LineEnding,
    /// Whether to enforce a single trailing newline.
    pub final_newline: bool,
    /// Whether to strip trailing whitespace on each line.
    pub trim_trailing_whitespace: bool,
}

impl Default for GlobalDefaults {
    fn default() -> Self {
        Self {
            line_length: 120,
            line_ending: LineEnding::Lf,
            final_newline: true,
            trim_trailing_whitespace: true,
        }
    }
}

/// On-disk representation of `[defaults]`. `line_ending` is a string so the TOML
/// stays human-friendly; it is normalized into [`LineEnding`] on load.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub(crate) struct RawDefaults {
    pub(crate) line_length: usize,
    pub(crate) final_newline: bool,
    pub(crate) trim_trailing_whitespace: bool,
    pub(crate) line_ending: String,
}

impl Default for RawDefaults {
    fn default() -> Self {
        Self {
            line_length: 120,
            final_newline: true,
            trim_trailing_whitespace: true,
            line_ending: "lf".to_string(),
        }
    }
}

impl From<RawDefaults> for GlobalDefaults {
    fn from(raw: RawDefaults) -> Self {
        let line_ending = match raw.line_ending.to_ascii_lowercase().as_str() {
            "crlf" => LineEnding::Crlf,
            _ => LineEnding::Lf,
        };
        GlobalDefaults {
            line_length: raw.line_length,
            line_ending,
            final_newline: raw.final_newline,
            trim_trailing_whitespace: raw.trim_trailing_whitespace,
        }
    }
}
