//! CSS / SCSS / Less formatting backend.
//!
//! Format-only — it does not produce lint diagnostics. The `lint` method returns an empty
//! `Vec` (the trait default).
//!
//! ## Formatter selection
//! By default the file goes through `oxc_formatter_css`, the formatter oxfmt itself uses
//! (Prettier-compatible), so a repository formatted by oxfmt is a fixed point. It reads the
//! shared layout keys `indent_width`, `line_width`, `line_ending`, `indent_style`,
//! `single_quote` and `trailing_commas` (`"always"` / `"never"`), which `.oxfmtrc.json[c]`
//! supplies by default. Setting any other key in the table (every [`malva`] option) selects
//! [`malva`] instead.
//!
//! ## Options layering (malva)
//! malva defaults → poly opinionated override (line length 120, indent 2) →
//! user `[fmt.css.malva]` / `[fmt.scss.malva]` / `[fmt.less.malva]`.
//!
//! The user table is deserialized into [`malva::config::FormatOptions`] (via
//! the `config_serde` feature).  All [`malva::config::LanguageOptions`] fields
//! are exposed.  The layout fields poly has a global for — `print_width`
//! (`[defaults] line_length`) and `line_break` (`[defaults] line_ending`) —
//! default to that global and are overridden when the user sets the key, per
//! the documented layering.  `indent_width` comes from
//! [`EngineConfig::indent_width`], itself resolved from the same table's
//! `indent_width` key.  `use_tabs` has no poly global and is user-controlled.

use malva::Syntax;
use malva::config::FormatOptions;
use oxc_allocator::Allocator;
use oxc_formatter_core::FormatOptions as _;
use oxc_formatter_css::{CssFormatOptions, CssVariant, TrailingCommas};

use super::core_options::core_format_options;

use crate::config::EngineConfig;
use crate::engine::{Capabilities, Engine, FormatOutput, OptionKeys, OptionTable, OptionType, SourceFile};
use crate::engine_guard::{deep_unbalanced_nesting, refuse_pathological_nesting};
use crate::engines::css_hacks::{IE_PROPERTY_HACK_SKIP, has_ie_property_hack};
use crate::language::Language;

/// malva CSS / SCSS / Less formatter backend.
pub struct MalvaEngine;

/// malva crate version — folded into the cache key so upgrades invalidate stale results.
/// Bumped suffix to +opts-1 after exposing full LanguageOptions (options were previously
/// ignored — existing caches must be invalidated).
const MALVA_VERSION: &str = "0.16.0+opts-2+iehack-skip+oxc_formatter_css-e2be72d+oxfmtrc-scope-1";

/// Keys the oxfmt-compatible formatter reads. The table is handed to malva instead as soon as it
/// holds anything else, because only malva understands the rest.
const SHARED_KEYS: &[&str] = &[
    "indent_width",
    "line_width",
    "line_ending",
    "indent_style",
    "single_quote",
    "trailing_commas",
    crate::oxfmt_scope::RC_KEY,
];

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

    /// The whole table is deserialized into `malva::config::FormatOptions`,
    /// so the recognised keys are derived from that type instead of copied out
    /// of it — the upstream option set is large, versioned, and would drift.
    fn option_keys(&self, table: OptionTable) -> OptionKeys {
        match table {
            OptionTable::Format => OptionKeys::declared(&[
                ("line_width", OptionType::INTEGER),
                ("line_ending", OptionType::STRING),
                ("indent_style", OptionType::STRING),
                ("single_quote", OptionType::BOOLEAN),
                ("trailing_commas", OptionType::STRING),
            ])
            .with_derived(crate::engines::config_keys::recognized_by_type_probe::<malva::config::FormatOptions>),
            OptionTable::Lint | OptionTable::CrossCuttingLint => OptionKeys::declared(&[]),
        }
    }

    fn version(&self) -> &str {
        MALVA_VERSION
    }

    fn format_config<'a>(&self, src: &SourceFile, cfg: &'a EngineConfig) -> std::borrow::Cow<'a, EngineConfig> {
        crate::oxfmt_scope::path_config(cfg, src, self.name())
    }

    fn format_input_salt(&self, path: &std::path::Path, cfg: &EngineConfig) -> Option<String> {
        crate::oxfmt_scope::input_salt(cfg, path)
    }

    fn file_context(&self, path: &std::path::Path, cfg: &EngineConfig) -> Option<String> {
        self.format_input_salt(path, cfg)
    }

    /// malva rejects the IE `*property` hack with a syntax error, so a legacy
    /// stylesheet nobody intends to modernise reported as unformattable on
    /// every run. A skip says the same thing without failing the check. See
    /// [`has_ie_property_hack`](crate::engines::css_hacks::has_ie_property_hack).
    fn skip_reason(&self, src: &SourceFile) -> Option<&'static str> {
        has_ie_property_hack(&src.content).then_some(IE_PROPERTY_HACK_SKIP)
    }

    fn format(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
        // Deeply nested brackets make CSS parsers backtrack exponentially (26 unclosed
        // parentheses take 25 s and 100 never return) or overflow the stack, wedging a
        // worker for the whole run, so both formatters are guarded.
        refuse_pathological_nesting(&src.path, &src.content)?;
        if deep_unbalanced_nesting(&src.content, MAX_UNCLOSED_NESTING) {
            anyhow::bail!(
                "malva: brackets nested more than {MAX_UNCLOSED_NESTING} levels deep are left unclosed; \
                 not parsed, because the parser takes exponential time on that"
            );
        }
        if uses_oxc_formatter(cfg) {
            return format_with_oxc(src, cfg);
        }
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

/// Unclosed nesting deeper than this is refused rather than parsed; closed
/// nesting is unaffected, and 18 unclosed levels still parse in about a second.
const MAX_UNCLOSED_NESTING: usize = 12;

/// Whether the table asks for nothing only the oxfmt-compatible formatter can serve.
fn uses_oxc_formatter(cfg: &EngineConfig) -> bool {
    cfg.options
        .keys()
        .all(|key| SHARED_KEYS.contains(&key.as_str()) || key == crate::engine::ENABLED_OPTION_KEY)
}

fn format_with_oxc(src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
    let variant = match src.language {
        Language::Scss => CssVariant::Scss,
        Language::Less => CssVariant::Less,
        _ => CssVariant::Css,
    };
    let allocator = Allocator::new();
    let formatted = match oxc_formatter_css::format(&allocator, &src.content, oxc_css_options(cfg, variant))
        .map_err(|_| ())
        .and_then(|formatted| formatted.print().map_err(|_| ()))
    {
        Ok(printed) => printed.into_code(),
        Err(()) => return Ok(FormatOutput::Unchanged),
    };
    if formatted == *src.content {
        Ok(FormatOutput::Unchanged)
    } else {
        Ok(FormatOutput::Formatted(formatted))
    }
}

/// The `oxc_formatter_css` options a `[fmt.<css|scss|less>.malva]` table resolves to.
pub(super) fn oxc_css_options(cfg: &EngineConfig, variant: CssVariant) -> CssFormatOptions {
    let mut options = CssFormatOptions {
        variant,
        ..CssFormatOptions::default()
    };
    options.apply_core(core_format_options(cfg));
    if let Some(single) = cfg.options.get("single_quote").and_then(toml::Value::as_bool) {
        options.single_quote = single.into();
    }
    if cfg.options.get("trailing_commas").and_then(toml::Value::as_str) == Some("never") {
        options.trailing_commas = TrailingCommas::Never;
    }
    options
}

/// Map a poly [`Language`] to the corresponding malva [`Syntax`].
fn language_to_syntax(lang: &Language) -> Option<Syntax> {
    match lang {
        Language::Css => Some(Syntax::Css),
        Language::Scss => Some(Syntax::Scss),
        Language::Less => Some(Syntax::Less),
        _ => None,
    }
}

/// Build [`FormatOptions`] from a poly [`EngineConfig`].
///
/// Layering:
/// 1. `FormatOptions::default()` — malva's own defaults.
/// 2. If `cfg.options` is non-empty, deserialise into `FormatOptions` via
///    `config_serde`; unknown keys are silently ignored.
/// 3. Override all `LayoutOptions` fields with poly's globals — these always
///    win over any layout keys the user may have placed in the options table.
fn build_options(cfg: &EngineConfig) -> FormatOptions {
    let mut options: FormatOptions = super::rule_config::deserialize_options(cfg, "[fmt.<css|scss|less>.malva]");

    // The opinionated globals are a layer *under* the user's table, not over it
    // (ADR 0006 / ADR 0007): they apply only where the user did not set the key.
    if !super::rule_config::sets_any(cfg, &["print_width", "printWidth"]) {
        options.layout.print_width = cfg.globals.line_length;
    }
    options.layout.indent_width = cfg.indent_width;
    if !super::rule_config::sets_any(cfg, &["line_break", "lineBreak", "linebreak"]) {
        options.layout.line_break = match cfg.globals.line_ending {
            crate::config::LineEnding::Crlf => malva::config::LineBreak::Crlf,
            crate::config::LineEnding::Lf => malva::config::LineBreak::Lf,
        };
    }
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
        assert!(diags.is_empty(), "malva is format-only; expected no diagnostics");
    }

    #[test]
    fn format_unchanged_when_already_formatted() {
        let engine = MalvaEngine;
        let src = make_src("clean.css", Language::Css, ".foo {\n  color: red;\n}\n");
        let result = engine.format(&src, &engine_cfg()).unwrap();
        assert!(
            matches!(result, FormatOutput::Unchanged),
            "already-canonical CSS should be Unchanged"
        );
    }

    #[test]
    fn unclosed_deep_nesting_is_refused_instead_of_wedging_the_parser() {
        let engine = MalvaEngine;
        let src = make_src(
            "broken.scss",
            Language::Scss,
            &format!("a {{ b: {}1 }}\n", "(".repeat(60)),
        );
        let error = engine
            .format(&src, &engine_cfg())
            .expect_err("unclosed nesting this deep must be refused, not parsed");
        assert!(error.to_string().contains("exponential"), "{error}");
    }

    #[test]
    fn nesting_deep_enough_to_overflow_the_parser_stack_is_refused_not_parsed() {
        let engine = MalvaEngine;
        let src = make_src(
            "deep.scss",
            Language::Scss,
            &format!("a {{ b: {}1{} }}\n", "(".repeat(1500), ")".repeat(1500)),
        );
        let error = engine
            .format(&src, &engine_cfg())
            .expect_err("too deep to parse safely");
        assert!(error.to_string().contains("nests brackets"), "{error}");
    }

    fn formatted(options: &str) -> String {
        let src = make_src("a.css", Language::Css, "a { background: var(--grad), var(--raise); }\n");
        let cfg = EngineConfig {
            options: toml::from_str(options).unwrap(),
            ..engine_cfg()
        };
        match MalvaEngine.format(&src, &cfg).unwrap() {
            FormatOutput::Formatted(out) => out,
            FormatOutput::Unchanged => src.content.to_string(),
        }
    }

    /// Prettier (and so oxfmt) keeps a short top-level comma list on one line; malva puts every
    /// entry on its own line. The default layout is Prettier's.
    #[test]
    fn default_formatter_matches_oxfmt_comma_list_layout() {
        assert_eq!(formatted(""), "a {\n  background: var(--grad), var(--raise);\n}\n");
    }

    #[test]
    fn a_malva_only_key_selects_malva() {
        assert_eq!(
            formatted("print_width = 100"),
            "a {\n  background:\n    var(--grad),\n    var(--raise);\n}\n"
        );
    }

    #[test]
    fn shared_keys_alone_keep_the_default_formatter() {
        assert!(formatted("indent_width = 4\nsingle_quote = true").contains("background: var(--grad), var(--raise);"));
    }
}
