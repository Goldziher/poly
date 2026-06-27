//! Configuration: the unified `poly.toml` (back-compat `polylint.toml`), parsed
//! by the [`poly_config`] crate and sliced per-engine here. Layering is: tool
//! defaults (inside each engine) → opinionated [`GlobalDefaults`] → user config.
//!
//! `polylint-core` consumes only the `[defaults]`, `[lint.*]`, and `[fmt.*]`
//! tables; the `[commit]` and `[hooks]` sections of the same file are read
//! directly from [`poly_config`] by the `poly commit` / `poly hooks` surfaces.

use std::path::Path;

// Re-exported so the rest of the crate (and downstream consumers) keep importing
// these from `polylint_core` / `crate::config` unchanged after the schema moved
// into the shared `poly-config` crate.
pub use poly_config::{GlobalDefaults, LineEnding};

use crate::language::Language;

/// Which phase a config slice is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Linting phase (`[lint.*]` tables).
    Lint,
    /// Formatting phase (`[fmt.*]` tables).
    Format,
}

/// The fully normalized configuration for the lint/format surfaces.
///
/// This is a thin projection of [`poly_config::PolyConfig`] onto the tables
/// `polylint-core` needs; the `[commit]` / `[hooks]` sections are intentionally
/// dropped here and consumed elsewhere.
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
    /// Load config by searching from `start` upward for `poly.toml` (or the
    /// back-compat `polylint.toml`).
    pub fn load(start: &Path) -> anyhow::Result<Config> {
        Ok(poly_config::PolyConfig::load(start)?.into())
    }

    /// Load config from an explicit file path.
    pub fn load_file(path: &Path) -> anyhow::Result<Config> {
        Ok(poly_config::PolyConfig::load_file(path)?.into())
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

impl From<poly_config::PolyConfig> for Config {
    fn from(pc: poly_config::PolyConfig) -> Self {
        Config {
            defaults: pc.defaults,
            lint: pc.lint,
            fmt: pc.fmt,
        }
    }
}
