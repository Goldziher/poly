//! Biome CSS lint backend.
//!
//! Wraps `biome_css_analyze::analyze` to run biome's CSS / SCSS lint rules
//! in-process. Capabilities: **lint-only**; the `MalvaEngine` holds the
//! format slot for CSS, SCSS, and Less and is unaffected.
//!
//! # Language coverage
//!
//! Handles `Language::Css` and `Language::Scss`.  `Language::Less` has no
//! biome parser support and is intentionally excluded; Less files are still
//! formatted by `MalvaEngine`.
//!
//! # Config table
//!
//! `[lint.css.biome]` / `[lint.scss.biome]` in `polylint.toml`.  Supports
//! the polylint uniform rule-selection surface (`select`, `extend_select`,
//! `ignore`, `[rules.<code>]`).
//!
//! # Opinionated defaults
//!
//! Groups `correctness` and `suspicious` are ON by default.
//! Group `nursery` is OFF (unstable rules).
//! All other groups (`a11y`, `style`, `complexity`, `performance`,
//! `security`) are opt-in via `extend_select`.
//!
//! # Engine name
//!
//! `"biome"` — matches the `[lint.css.biome]` config key and the
//! `Diagnostic.engine` field surfaced in reports.

use biome_analyze::{AnalyzerOptions, ControlFlow, Never};
use biome_css_analyze::CssAnalyzerServices;
use biome_css_parser::{CssParserOptions, parse_css};
use biome_languages::CssFileSource;

use crate::config::EngineConfig;
use crate::engine::{Capabilities, Diagnostic, Engine, SourceFile};
use crate::engines::biome_common::{build_lint_filter, map_biome_diag, rule_filter_strings, str_to_rule_filter};
use crate::language::Language;

/// Biome CSS lint engine.
///
/// Runs biome's `correctness` + `suspicious` CSS rule groups by default.
/// Nursery rules are off; opt in with `extend_select = ["nursery"]` in
/// `[lint.css.biome]`.  Less is not supported by biome and is excluded.
pub struct BiomeCssEngine;

static LANGUAGES: &[Language] = &[Language::Css, Language::Scss];

/// Opinionated default rule groups: real correctness bugs + suspicious patterns.
/// Nursery is excluded (unstable). a11y / style / complexity / performance /
/// security are opt-in.
const DEFAULT_GROUPS: &[&str] = &["correctness", "suspicious"];

/// Cache-key version string.  Embeds the pinned biome rev so the blake3 cache
/// is invalidated whenever the rev changes.  Bump the `+lint-vN` suffix when
/// the diagnostic mapping logic changes output for identical input.
const VERSION: &str = "biome_css_analyze+rev:93d8e53+lint-v1";

/// Map a polylint [`Language`] to the biome [`CssFileSource`].
///
/// SCSS maps to `CssFileSource::scss()`.  All other supported languages
/// (currently only CSS) map to `CssFileSource::css()`.
fn css_file_source(lang: &Language) -> CssFileSource {
    match lang {
        Language::Scss => CssFileSource::scss(),
        _ => CssFileSource::css(),
    }
}

impl Engine for BiomeCssEngine {
    fn name(&self) -> &'static str {
        "biome"
    }

    fn languages(&self) -> &'static [Language] {
        LANGUAGES
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            lint: true,
            format: false,
            // Autofix is not wired for v1 — biome's mutation API requires
            // diffing the committed AST against the original source to produce
            // byte-range Edits, deferred to a follow-up.
            fix: false,
        }
    }

    fn version(&self) -> &str {
        VERSION
    }

    fn lint(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        let file_source = css_file_source(&src.language);
        let parsed = parse_css(&src.content, file_source, CssParserOptions::default());
        let root = parsed.tree();

        // Build the rule filter string lists; both vecs must remain alive
        // through the `analyze()` call so `AnalysisFilter`'s borrowed
        // `&[RuleFilter<'_>]` pointers remain valid.
        let (enabled_strs, disabled_strs) = rule_filter_strings(cfg, DEFAULT_GROUPS);
        let enabled_filters: Vec<_> = enabled_strs.iter().map(|s| str_to_rule_filter(s)).collect();
        let disabled_filters: Vec<_> = disabled_strs.iter().map(|s| str_to_rule_filter(s)).collect();
        let filter = build_lint_filter(&enabled_filters, &disabled_filters);

        // Services: `semantic_model = None` causes `analyze()` to build the
        // semantic model internally (biome_css_analyze lib.rs:~180).
        // No ProjectLayout or ModuleDb needed for per-file lint.
        let css_services = CssAnalyzerServices::default().with_file_source(file_source);

        let options = AnalyzerOptions::default();
        let mut out: Vec<Diagnostic> = Vec::new();

        // `plugins = &[]` — no biome plugin extensions; we only run built-in rules.
        // `analyze` returns `(Option<Never>, Vec<Error>)` when the closure never breaks.
        let (_, _parse_errors) = biome_css_analyze::analyze(&root, filter, &options, css_services, &[], |signal| {
            if let Some(diag) = signal.diagnostic() {
                out.push(map_biome_diag(&diag, &src.content, "biome"));
            }
            ControlFlow::<Never>::Continue(())
        });

        Ok(out)
    }

    // format() uses the Engine trait default (returns FormatOutput::Unchanged).
    // MalvaEngine holds the format slot for Language::Css, Scss, and Less.
}

#[cfg(test)]
mod tests {
    use crate::config::GlobalDefaults;
    use crate::engine::Engine;
    use crate::language::Language;

    use super::*;

    fn default_cfg() -> EngineConfig {
        EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 2,
            options: toml::Table::new(),
        }
    }

    #[test]
    fn engine_metadata() {
        let engine = BiomeCssEngine;
        assert_eq!(engine.name(), "biome");
        assert!(engine.capabilities().lint);
        assert!(!engine.capabilities().format);
        assert!(!engine.capabilities().fix);
        assert!(engine.languages().contains(&Language::Css));
        assert!(engine.languages().contains(&Language::Scss));
        assert!(!engine.languages().contains(&Language::Less));
    }

    #[test]
    fn valid_css_produces_no_diagnostics() {
        let engine = BiomeCssEngine;
        let src = crate::engine::SourceFile {
            path: "test.css".into(),
            language: Language::Css,
            content: "a { color: blue; }\n".into(),
        };
        let diags = engine.lint(&src, &default_cfg()).unwrap();
        assert!(diags.is_empty(), "expected no diagnostics; got: {diags:#?}");
    }
}
