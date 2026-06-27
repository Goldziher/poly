//! Markup backend: HTML / Vue / Svelte formatting via [`markup_fmt`].
//!
//! Capabilities: [`Capabilities::format`] only — markup_fmt is a formatter and
//! does not report diagnostics.
//!
//! ## Embedded code (v1 limitation)
//!
//! markup_fmt delegates embedded `<script>` / `<style>` blocks to an external
//! formatter callback. polylint passes a no-op callback for now, so embedded
//! JS/CSS is left untouched; a later milestone can route those blocks through
//! the oxc / malva backends.
//!
//! ## Options layering
//! markup_fmt defaults → polylint opinionated override (print_width 120,
//! indent_width from the language default, which is 2 for these markup
//! languages) → user `[fmt.html.markup_fmt]` / `[fmt.vue.markup_fmt]` /
//! `[fmt.svelte.markup_fmt]`.

use markup_fmt::Language as MarkupLanguage;
use markup_fmt::config::{FormatOptions, LayoutOptions};
use markup_fmt::format_text;

use crate::config::EngineConfig;
use crate::engine::{Capabilities, Engine, FormatOutput, SourceFile};
use crate::language::Language;

/// markup_fmt HTML / Vue / Svelte formatter backend.
pub struct MarkupFmtEngine;

/// markup_fmt crate version — folded into the cache key so upgrades invalidate
/// any stale cached output.
const VERSION: &str = "0.27.3";

/// Languages handled by this backend.
static LANGUAGES: &[Language] = &[Language::Html, Language::Vue, Language::Svelte];

impl Engine for MarkupFmtEngine {
    fn name(&self) -> &'static str {
        "markup_fmt"
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
        VERSION
    }

    fn format(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
        let Some(language) = markup_language(&src.language) else {
            return Ok(FormatOutput::Unchanged);
        };

        let options = FormatOptions {
            layout: LayoutOptions {
                print_width: cfg.globals.line_length,
                indent_width: cfg.indent_width,
                ..LayoutOptions::default()
            },
            ..FormatOptions::default()
        };

        // No-op external formatter: embedded <script>/<style> blocks pass
        // through untouched (v1 limitation). The closure's error type is
        // markup_fmt's `anyhow::Error`, inferred from `format_text`.
        let formatted = format_text(&src.content, language, &options, |code, _| Ok(code.into()))
            .map_err(|e| anyhow::anyhow!("markup_fmt error: {e}"))?;

        if formatted == *src.content {
            Ok(FormatOutput::Unchanged)
        } else {
            Ok(FormatOutput::Formatted(formatted))
        }
    }
}

/// Map a polylint [`Language`] to the corresponding markup_fmt [`MarkupLanguage`].
fn markup_language(lang: &Language) -> Option<MarkupLanguage> {
    match lang {
        Language::Html => Some(MarkupLanguage::Html),
        Language::Vue => Some(MarkupLanguage::Vue),
        Language::Svelte => Some(MarkupLanguage::Svelte),
        _ => None,
    }
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
        let engine = MarkupFmtEngine;
        assert_eq!(engine.name(), "markup_fmt");
        assert!(engine.languages().contains(&Language::Html));
        assert!(engine.languages().contains(&Language::Vue));
        assert!(engine.languages().contains(&Language::Svelte));
        let caps = engine.capabilities();
        assert!(!caps.lint);
        assert!(caps.format);
        assert!(!caps.fix);
    }

    #[test]
    fn unsupported_language_is_unchanged() {
        let engine = MarkupFmtEngine;
        let src = make_src("x.txt", Language::Other("text".into()), "hello\n");
        assert!(matches!(
            engine.format(&src, &engine_cfg()).unwrap(),
            FormatOutput::Unchanged
        ));
    }
}
