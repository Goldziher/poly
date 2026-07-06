//! Markup backend: HTML / Vue / Svelte / Astro / Angular / Jinja / Vento /
//! Mustache / XML formatting via [`markup_fmt`].
//!
//! Capabilities: [`Capabilities::format`] only — markup_fmt is a formatter and
//! does not report diagnostics.
//!
//! ## Embedded code (v1 limitation)
//!
//! markup_fmt delegates embedded `<script>` / `<style>` blocks to an external
//! formatter callback. poly passes a no-op callback for now, so embedded
//! JS/CSS is left untouched; a later milestone can route those blocks through
//! the oxc / malva backends.
//!
//! ## Angular detection
//! Angular templates share the `.html` extension with plain HTML. poly
//! follows markup_fmt's own `detect_language` heuristic: a file whose stem
//! ends with `.component` (e.g. `app.component.html`) is routed to
//! `Language::Angular`; all other `.html` files go to `Language::Html`.
//!
//! ## Jinja covers Twig / Nunjucks
//! markup_fmt v0.27 exposes a single `Jinja` variant that handles Jinja2,
//! Twig, and Nunjucks templates. Extensions `.jinja`, `.jinja2`, `.j2`,
//! `.twig`, and `.njk` all route here.
//!
//! ## Mustache covers Handlebars
//! Similarly, `.mustache`, `.hbs`, and `.handlebars` all route to the
//! `Mustache` variant.
//!
//! ## Options layering
//! markup_fmt defaults → poly opinionated override (print_width 120,
//! indent_width 2 for all markup languages) → user
//! `[fmt.<lang>.markup_fmt]`.
//!
//! The user table is deserialized into [`markup_fmt::config::FormatOptions`]
//! (via the `config_serde` feature).  All
//! [`markup_fmt::config::LanguageOptions`] fields are exposed.  Layout fields
//! (`print_width`, `indent_width`, `line_break`, `use_tabs`) are always taken
//! from poly globals and override anything in the options table.

use markup_fmt::Language as MarkupLanguage;
use markup_fmt::config::FormatOptions;
use markup_fmt::format_text;

use crate::config::EngineConfig;
use crate::engine::{Capabilities, Engine, FormatOutput, SourceFile};
use crate::language::Language;

/// markup_fmt HTML / Vue / Svelte / Astro / Angular / Jinja / Vento /
/// Mustache / XML formatter backend.
pub struct MarkupFmtEngine;

/// markup_fmt crate version — folded into the cache key so upgrades invalidate
/// any stale cached output.
/// Bumped suffix to +opts-1 after exposing full LanguageOptions (options were
/// previously ignored — existing caches must be invalidated).
const VERSION: &str = "0.27.3+opts-1";

/// Languages handled by this backend.
static LANGUAGES: &[Language] = &[
    Language::Html,
    Language::Vue,
    Language::Svelte,
    Language::Astro,
    Language::Angular,
    Language::Jinja,
    Language::Vento,
    Language::Mustache,
    Language::Xml,
];

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

        let options = build_options(cfg);

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

/// Map a poly [`Language`] to the corresponding markup_fmt [`MarkupLanguage`].
fn markup_language(lang: &Language) -> Option<MarkupLanguage> {
    match lang {
        Language::Html => Some(MarkupLanguage::Html),
        Language::Vue => Some(MarkupLanguage::Vue),
        Language::Svelte => Some(MarkupLanguage::Svelte),
        Language::Astro => Some(MarkupLanguage::Astro),
        Language::Angular => Some(MarkupLanguage::Angular),
        Language::Jinja => Some(MarkupLanguage::Jinja),
        Language::Vento => Some(MarkupLanguage::Vento),
        Language::Mustache => Some(MarkupLanguage::Mustache),
        Language::Xml => Some(MarkupLanguage::Xml),
        _ => None,
    }
}

/// Build [`FormatOptions`] from a poly [`EngineConfig`].
///
/// Layering:
/// 1. `FormatOptions::default()` — markup_fmt's own defaults.
/// 2. If `cfg.options` is non-empty, deserialise into `FormatOptions` via
///    `config_serde`; unknown keys are silently ignored.
/// 3. Override all `LayoutOptions` fields with poly's globals — these always
///    win over any layout keys the user may have placed in the options table.
fn build_options(cfg: &EngineConfig) -> FormatOptions {
    let mut options: FormatOptions =
        super::rule_config::deserialize_options(cfg, "[fmt.<html|vue|svelte|…>.markup_fmt]");

    // Poly's layout always wins — these come from globals, not the user table.
    // (use_tabs has no global, so it stays user-controllable from the table.)
    options.layout.print_width = cfg.globals.line_length;
    options.layout.indent_width = cfg.indent_width;
    options.layout.line_break = match cfg.globals.line_ending {
        crate::config::LineEnding::Crlf => markup_fmt::config::LineBreak::Crlf,
        crate::config::LineEnding::Lf => markup_fmt::config::LineBreak::Lf,
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
        let engine = MarkupFmtEngine;
        assert_eq!(engine.name(), "markup_fmt");
        for lang in &[
            Language::Html,
            Language::Vue,
            Language::Svelte,
            Language::Astro,
            Language::Angular,
            Language::Jinja,
            Language::Vento,
            Language::Mustache,
            Language::Xml,
        ] {
            assert!(
                engine.languages().contains(lang),
                "{lang:?} should be listed in MarkupFmtEngine::languages()"
            );
        }
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
