//! Configuration: canonical `polylint.toml`, normalized into a [`Config`] and
//! sliced per-engine. Layering is: tool defaults (inside each engine) →
//! opinionated [`GlobalDefaults`] → user `polylint.toml`.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::language::Language;

/// Which phase a config slice is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Linting phase (`[lint.*]` tables).
    Lint,
    /// Formatting phase (`[fmt.*]` tables).
    Format,
}

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

/// The fully normalized configuration.
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// Global opinionated defaults.
    pub defaults: GlobalDefaults,
    /// `[lint.<lang>.<tool>]` tables.
    pub lint: toml::Table,
    /// `[fmt.<lang>.<tool>]` tables.
    pub fmt: toml::Table,
}

/// The slice of config handed to one engine for one file.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Global opinionated defaults.
    pub globals: GlobalDefaults,
    /// Indent width for this file's language.
    pub indent_width: usize,
    /// Tool-specific options from `[<kind>.<lang>.<engine>]` (engine merges its own defaults).
    pub options: toml::Table,
}

impl Config {
    /// Load config by searching from `start` upward for `polylint.toml`.
    pub fn load(start: &Path) -> anyhow::Result<Config> {
        match find_config(start) {
            Some(path) => Config::load_file(&path),
            None => Ok(Config::default()),
        }
    }

    /// Load config from an explicit file path.
    pub fn load_file(path: &Path) -> anyhow::Result<Config> {
        let text = std::fs::read_to_string(path)?;
        let raw: RawConfig = toml::from_str(&text)?;
        Ok(raw.into())
    }

    /// Build the [`EngineConfig`] slice for a given language + engine + phase.
    pub fn engine_config(&self, lang: &Language, engine_name: &str, kind: Kind) -> EngineConfig {
        let tables = match kind {
            Kind::Lint => &self.lint,
            Kind::Format => &self.fmt,
        };
        let options = tables
            .get(lang.id())
            .and_then(|v| v.as_table())
            .and_then(|t| t.get(engine_name))
            .and_then(|v| v.as_table())
            .cloned()
            .unwrap_or_default();
        EngineConfig {
            globals: self.defaults.clone(),
            indent_width: lang.default_indent_width(),
            options,
        }
    }
}

fn find_config(start: &Path) -> Option<PathBuf> {
    let mut dir = if start.is_file() {
        start.parent()?
    } else {
        start
    };
    loop {
        let candidate = dir.join("polylint.toml");
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = dir.parent()?;
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct RawConfig {
    defaults: RawDefaults,
    lint: toml::Table,
    fmt: toml::Table,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
struct RawDefaults {
    line_length: usize,
    final_newline: bool,
    trim_trailing_whitespace: bool,
    line_ending: String,
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

impl From<RawConfig> for Config {
    fn from(raw: RawConfig) -> Self {
        let line_ending = match raw.defaults.line_ending.to_ascii_lowercase().as_str() {
            "crlf" => LineEnding::Crlf,
            _ => LineEnding::Lf,
        };
        Config {
            defaults: GlobalDefaults {
                line_length: raw.defaults.line_length,
                line_ending,
                final_newline: raw.defaults.final_newline,
                trim_trailing_whitespace: raw.defaults.trim_trailing_whitespace,
            },
            lint: raw.lint,
            fmt: raw.fmt,
        }
    }
}
