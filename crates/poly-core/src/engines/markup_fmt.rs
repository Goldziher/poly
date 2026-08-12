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
const VERSION: &str = "0.27.3+opts-1+tmpltarget";

/// Reason reported when a general-purpose template does not render markup.
const NON_MARKUP_TEMPLATE_SKIP: &str = "template does not render markup";

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

    fn skip_reason(&self, src: &SourceFile) -> Option<&'static str> {
        (is_generic_template(&src.language) && !targets_markup(&src.path, &src.content))
            .then_some(NON_MARKUP_TEMPLATE_SKIP)
    }

    fn format(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
        let Some(language) = markup_language(&src.language) else {
            return Ok(FormatOutput::Unchanged);
        };

        if self.skip_reason(src).is_some() {
            return Ok(FormatOutput::Unchanged);
        }

        let options = build_options(cfg);

        let formatted = format_text(&src.content, language, &options, |code, _| Ok(code.into()))
            .map_err(|e| anyhow::anyhow!("markup_fmt error: {e}"))?;

        if formatted == *src.content {
            Ok(FormatOutput::Unchanged)
        } else {
            Ok(FormatOutput::Formatted(formatted))
        }
    }
}

/// Whether this language is a *general-purpose* template syntax, i.e. one whose
/// rendered output is not necessarily markup.
///
/// Jinja, Vento and Mustache are routinely used to generate Go, Python, SQL and
/// other whitespace-sensitive source. `.html`/`.vue`/`.svelte`/`.astro`/`.xml`
/// are unambiguous and never need the check.
fn is_generic_template(language: &Language) -> bool {
    matches!(language, Language::Jinja | Language::Vento | Language::Mustache)
}

/// Whether a general-purpose template appears to render markup.
///
/// markup_fmt reflows on the assumption that whitespace is insignificant, which
/// is true of HTML and false of most other languages. Applied to a template that
/// renders Go, it joined statements onto one line — turning
/// `data, err := json.Marshal(r)` followed by `if err != nil {` into
/// `json.Marshal(r) if err != nil` and emitting source that does not compile.
///
/// Two signals, cheapest first:
///
/// 1. A double extension names the target directly (`marshal.go.jinja`), so a
///    non-markup target is decisive regardless of content.
/// 2. Otherwise the body must contain a literal tag once template constructs are
///    removed — `{{ ... }}` can hold a `<` that is not markup at all.
///
/// Declining is the safe default here: leaving a template unformatted costs
/// nothing, while reflowing one destroys it.
fn targets_markup(path: &std::path::Path, content: &str) -> bool {
    if let Some(stem) = path.file_stem().and_then(|s| s.to_str())
        && let Some((_, inner)) = stem.rsplit_once('.')
    {
        let inner = inner.to_ascii_lowercase();
        return matches!(
            inner.as_str(),
            "html" | "htm" | "xhtml" | "xml" | "svg" | "vue" | "svelte" | "astro"
        );
    }
    contains_markup_tag(content)
}

/// Whether `content` contains a literal markup tag outside template constructs.
///
/// Strips `{{ … }}`, `{% … %}` and `{# … #}` first so an expression such as
/// `{% if a < b %}` is not mistaken for a tag, then looks for `<name` or
/// `</name` with an ASCII-alphabetic tag name.
fn contains_markup_tag(content: &str) -> bool {
    let mut stripped = String::with_capacity(content.len());
    let bytes = content.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let rest = &content[index..];
        if let Some(close) = template_construct_end(rest) {
            index += close;
            continue;
        }
        let ch = rest.chars().next().unwrap_or('\0');
        stripped.push(ch);
        index += ch.len_utf8();
    }

    let bytes = stripped.as_bytes();
    bytes.iter().enumerate().any(|(i, &b)| {
        if b != b'<' {
            return false;
        }
        let mut next = i + 1;
        if bytes.get(next) == Some(&b'/') {
            next += 1;
        }
        bytes.get(next).is_some_and(u8::is_ascii_alphabetic)
    })
}

/// Length of the template construct starting at `rest`, if it starts with one.
fn template_construct_end(rest: &str) -> Option<usize> {
    for (open, close) in [("{{", "}}"), ("{%", "%}"), ("{#", "#}")] {
        if let Some(body) = rest.strip_prefix(open) {
            return Some(
                body.find(close)
                    .map_or(rest.len(), |end| open.len() + end + close.len()),
            );
        }
    }
    None
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

    /// A Jinja template rendering Go must be left alone. markup_fmt reflows on
    /// the assumption that whitespace is insignificant, which turned
    /// `data, err := json.Marshal(r)` + `if err != nil {` into
    /// `json.Marshal(r) if err != nil` — source that does not compile.
    #[test]
    fn template_rendering_non_markup_is_left_alone() {
        let go_template = concat!(
            "func (r *{{ type_name }}) MarshalC() ([]byte, error) {\n",
            "\tdata, err := json.Marshal(r)\n",
            "\tif err != nil {\n",
            "\t\treturn nil, err\n",
            "\t}\n",
            "\treturn data, nil\n",
            "}\n",
        );
        let src = make_src("marshal_receiver_to_c.jinja", Language::Jinja, go_template);

        assert!(matches!(
            MarkupFmtEngine.format(&src, &engine_cfg()).expect("format"),
            FormatOutput::Unchanged
        ));
    }

    /// The guard must not cost genuine HTML templates their formatting.
    #[test]
    fn template_rendering_markup_is_still_formatted() {
        let src = make_src(
            "page.jinja",
            Language::Jinja,
            "<div    class=\"a\">\n<p>{{ name }}</p>\n</div>\n",
        );

        assert!(matches!(
            MarkupFmtEngine.format(&src, &engine_cfg()).expect("format"),
            FormatOutput::Formatted(_)
        ));
    }

    /// A double extension names the target outright and outranks content.
    #[test]
    fn double_extension_decides_the_target() {
        let go = make_src("tmpl.go.jinja", Language::Jinja, "func F() {\n\tx := 1\n}\n");
        assert!(matches!(
            MarkupFmtEngine.format(&go, &engine_cfg()).expect("format"),
            FormatOutput::Unchanged
        ));

        let html = make_src("page.html.jinja", Language::Jinja, "<div    class=\"a\">\n</div>\n");
        assert!(matches!(
            MarkupFmtEngine.format(&html, &engine_cfg()).expect("format"),
            FormatOutput::Formatted(_)
        ));
    }

    /// `<` inside a template expression is a comparison, not a tag.
    #[test]
    fn angle_bracket_inside_template_expression_is_not_a_tag() {
        assert!(!contains_markup_tag("{% if a < b %}\nvalue = {{ a }}\n{% endif %}\n"));
        assert!(contains_markup_tag("{% if a < b %}<span>x</span>{% endif %}"));
    }

    /// Unambiguous markup languages skip the check entirely.
    #[test]
    fn dedicated_markup_languages_are_not_sniffed() {
        for language in [Language::Html, Language::Vue, Language::Svelte, Language::Xml] {
            assert!(!is_generic_template(&language), "{language:?} needs no target check");
        }
        for language in [Language::Jinja, Language::Vento, Language::Mustache] {
            assert!(is_generic_template(&language), "{language:?} must be checked");
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
