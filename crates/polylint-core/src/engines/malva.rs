//! malva backend: CSS / SCSS / Less formatting via [`malva`].
//!
//! malva is a format-only backend — it does not produce lint diagnostics.
//! The `lint` method returns an empty `Vec` (the trait default).
//!
//! ## Options layering
//! malva defaults → polylint opinionated override (line length 120, indent 2) →
//! user `[fmt.css.malva]` / `[fmt.scss.malva]` / `[fmt.less.malva]`.
//!
//! The user table is deserialized into [`malva::config::FormatOptions`] (via
//! the `config_serde` feature).  All [`malva::config::LanguageOptions`] fields
//! are exposed.  Layout fields (`print_width`, `indent_width`, `line_break`,
//! `use_tabs`) are always taken from poly globals and override anything the
//! user places in the options table.

use malva::Syntax;
use malva::config::FormatOptions;

use crate::config::EngineConfig;
use crate::engine::{Capabilities, Engine, FormatOutput, SourceFile};
use crate::language::Language;

/// malva CSS / SCSS / Less formatter backend.
pub struct MalvaEngine;

/// malva crate version — folded into the cache key so upgrades invalidate stale results.
/// Bumped suffix to +opts-1 after exposing full LanguageOptions (options were previously
/// ignored — existing caches must be invalidated).
const MALVA_VERSION: &str = "0.16.0+opts-1";

/// Languages handled by this backend.
static LANGUAGES: &[Language] = &[Language::Css, Language::Scss, Language::Less];

impl Engine for MalvaEngine {
    fn name(&self) -> &'static str {
        "malva"
    }

    fn languages(&self) -> &'static [Language] {
        LANGUAGES
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            lint: false,
            format: true,
            fix: false,
        }
    }

    fn version(&self) -> &str {
        MALVA_VERSION
    }

    fn format(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
        let syntax = language_to_syntax(&src.language)
            .ok_or_else(|| anyhow::anyhow!("malva: unsupported language {:?}", src.language))?;
        let options = build_options(cfg);
        let formatted = malva::format_text(&src.content, syntax, &options)
            .map_err(|e| anyhow::anyhow!("malva format error: {e}"))?;
        if formatted == *src.content {
            Ok(FormatOutput::Unchanged)
        } else {
            Ok(FormatOutput::Formatted(formatted))
        }
    }
}

/// Map a polylint [`Language`] to the corresponding malva [`Syntax`].
fn language_to_syntax(lang: &Language) -> Option<Syntax> {
    match lang {
        Language::Css => Some(Syntax::Css),
        Language::Scss => Some(Syntax::Scss),
        Language::Less => Some(Syntax::Less),
        _ => None,
    }
}

/// Build [`FormatOptions`] from a polylint [`EngineConfig`].
///
/// Layering:
/// 1. `FormatOptions::default()` — malva's own defaults.
/// 2. If `cfg.options` is non-empty, deserialise into `FormatOptions` via
///    `config_serde`; unknown keys are silently ignored.
/// 3. Override all `LayoutOptions` fields with poly's globals — these always
///    win over any layout keys the user may have placed in the options table.
fn build_options(cfg: &EngineConfig) -> FormatOptions {
    let mut options: FormatOptions =
        super::rule_config::deserialize_options(cfg, "[fmt.<css|scss|less>.malva]");

    // Poly's layout always wins — these come from globals, not the user table.
    // (use_tabs has no global, so it stays user-controllable from the table.)
    options.layout.print_width = cfg.globals.line_length;
    options.layout.indent_width = cfg.indent_width;
    options.layout.line_break = match cfg.globals.line_ending {
        crate::config::LineEnding::Crlf => malva::config::LineBreak::Crlf,
        crate::config::LineEnding::Lf => malva::config::LineBreak::Lf,
    };
    options
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::config::GlobalDefaults;

    fn engine_cfg() -> EngineConfig {
        EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 2,
            options: toml::Table::new(),
        }
    }

    fn make_src(path: &str, language: Language, content: &str) -> SourceFile {
        SourceFile {
            path: PathBuf::from(path),
            language,
            content: content.into(),
        }
    }

    #[test]
    fn engine_metadata() {
        let engine = MalvaEngine;
        assert_eq!(engine.name(), "malva");
        assert!(engine.languages().contains(&Language::Css));
        assert!(engine.languages().contains(&Language::Scss));
        assert!(engine.languages().contains(&Language::Less));
        let caps = engine.capabilities();
        assert!(!caps.lint);
        assert!(caps.format);
        assert!(!caps.fix);
    }

    #[test]
    fn lint_returns_empty_diagnostics() {
        let engine = MalvaEngine;
        let src = make_src("test.css", Language::Css, ".foo { color: red; }\n");
        let diags = engine.lint(&src, &engine_cfg()).unwrap();
        assert!(
            diags.is_empty(),
            "malva is format-only; expected no diagnostics"
        );
    }

    #[test]
    fn format_unchanged_when_already_formatted() {
        let engine = MalvaEngine;
        // malva's canonical output for a single rule.
        let src = make_src("clean.css", Language::Css, ".foo {\n  color: red;\n}\n");
        let result = engine.format(&src, &engine_cfg()).unwrap();
        assert!(
            matches!(result, FormatOutput::Unchanged),
            "already-canonical CSS should be Unchanged"
        );
    }
}
