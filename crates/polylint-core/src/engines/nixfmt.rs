//! Nix backend: formatting via [`alejandra`].
//!
//! alejandra is a fully opinionated Nix formatter (two-space indent, no
//! configurable line length) built on the pure-Rust `rnix` parser. polylint
//! respects its output as-is — the same way `gofmt` is respected — so there is
//! no override layer here. Format-only: no lint, no fix.
//!
//! nixpkgs-fmt was evaluated first but transitively pulls `ansi_term` and
//! `atty`, both flagged unmaintained by `cargo deny` (RUSTSEC-2021-0139 /
//! RUSTSEC-2024-0375). alejandra (UNLICENSE) has a clean advisory tree.

use alejandra::format::{Status, in_memory};

use crate::config::EngineConfig;
use crate::engine::{Capabilities, Diagnostic, Engine, FormatOutput, SourceFile};
use crate::language::Language;

/// alejandra Nix backend — format-only for `.nix` files.
pub struct NixFmtEngine;

/// alejandra crate version; folded into the cache key so upgrades invalidate
/// any stale cached output.
const VERSION: &str = "3.1.0";

/// Languages handled by this backend.
static LANGUAGES: &[Language] = &[Language::Nix];

impl Engine for NixFmtEngine {
    fn name(&self) -> &'static str {
        "alejandra"
    }

    fn languages(&self) -> &'static [Language] {
        LANGUAGES
    }

    /// Format-only: alejandra neither reports diagnostics nor applies fixes.
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            lint: false,
            format: true,
            fix: false,
        }
    }

    fn version(&self) -> &str {
        VERSION
    }

    /// No-op: alejandra has no lint capability.
    fn lint(&self, _src: &SourceFile, _cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        Ok(vec![])
    }

    /// Format `src.content` with alejandra. Returns [`FormatOutput::Unchanged`]
    /// when the output equals the input (already formatted) and when the input
    /// cannot be parsed, so unparsable Nix is never corrupted.
    fn format(&self, src: &SourceFile, _cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
        // `in_memory` takes ownership of both strings; the clone is at the API
        // boundary (alejandra parses `before` into an rnix tree internally).
        // alejandra is fully opinionated (two-space indent) and exposes no
        // configuration in this version.
        let (status, after) =
            in_memory(src.path.to_string_lossy().into_owned(), src.content.clone());
        match status {
            // Parse error: leave the file untouched rather than risk data loss.
            Status::Error(_) => Ok(FormatOutput::Unchanged),
            Status::Changed(false) => Ok(FormatOutput::Unchanged),
            Status::Changed(true) => Ok(FormatOutput::Formatted(after)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn make_src(content: &str) -> SourceFile {
        SourceFile {
            path: PathBuf::from("test.nix"),
            language: Language::Nix,
            content: content.to_string(),
        }
    }

    fn default_cfg() -> EngineConfig {
        use crate::config::GlobalDefaults;
        EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 4,
            options: toml::Table::new(),
        }
    }

    #[test]
    fn engine_metadata() {
        let engine = NixFmtEngine;
        assert_eq!(engine.name(), "alejandra");
        assert_eq!(engine.languages(), &[Language::Nix]);
        let caps = engine.capabilities();
        assert!(!caps.lint);
        assert!(caps.format);
        assert!(!caps.fix);
        assert_eq!(engine.version(), VERSION);
    }

    #[test]
    fn lint_always_returns_empty() {
        let engine = NixFmtEngine;
        let src = make_src("{ foo = 1; }");
        let diags = engine.lint(&src, &default_cfg()).unwrap();
        assert!(diags.is_empty());
    }

    #[test]
    fn unformatted_input_returns_formatted() {
        let engine = NixFmtEngine;
        let src = make_src("{foo=42;}");
        let out = engine.format(&src, &default_cfg()).unwrap();
        assert!(
            matches!(out, FormatOutput::Formatted(_)),
            "expected Formatted for unformatted input"
        );
    }

    #[test]
    fn unparsable_input_is_unchanged() {
        let engine = NixFmtEngine;
        // A bare `{` never closes — alejandra reports a parse error; we must
        // leave the content untouched rather than corrupt it.
        let src = make_src("{ this is not valid nix");
        let out = engine.format(&src, &default_cfg()).unwrap();
        assert!(matches!(out, FormatOutput::Unchanged));
    }
}
