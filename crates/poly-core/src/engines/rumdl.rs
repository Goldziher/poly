//! rumdl backend — Markdown lint and auto-format.
//!
//! Wraps [`rumdl_lib`](https://crates.io/crates/rumdl) in-process — no subprocess, no system
//! dependency. Capabilities: lint (standard markdownlint rules; rumdl-proprietary stylistic
//! extensions off by default — see the `DEFAULT_DISABLED_RULES` constant) + format (apply all
//! auto-fixable rules iteratively until convergence).
//!
//! Config layering: rumdl defaults → opinionated override (line-length 120) → user
//! `[lint.markdown.rumdl]` / `[fmt.markdown.rumdl]` table in `poly.toml`.
//!
//! Rule selection accepts the canonical vocabulary (ADR 0016): `select` /
//! `extend_select` map onto rumdl's `enable`, and `ignore` maps onto `disable`.
//! The native `enable` / `disable` keys remain accepted as aliases and are
//! unioned with the canonical keys.

use std::collections::HashSet;

use rumdl_lib::{
    config::{Config as RumdlConfig, MarkdownFlavor},
    fix_coordinator::FixCoordinator,
    rule::{LintWarning, Rule, RuleCategory, Severity as RumdlSeverity},
    rules::{all_rules, filter_rules},
    types::LineLength,
};

use super::rule_config::{RuleSelection, string_list, union_codes, warn_and_skip_blank};
use super::template::contains_go_template;
use crate::config::EngineConfig;
use crate::engine::{Capabilities, Diagnostic, Edit, Engine, FormatOutput, Severity, SourceFile, Span};
use crate::language::Language;

/// rumdl Markdown lint + format backend.
pub struct RumdlEngine;

/// Embedded crate version so the cache key changes whenever rumdl output could change.
/// The `+defaults3` suffix marks the opinionated rule policy below: the
/// default-disabled proprietary rules **and** the lint-mode suppression of the
/// `Whitespace` (formatting) category, plus MDX-flavor routing and the
/// Go/Helm-template skip. Bump the suffix whenever any of these change so stale
/// cached diagnostics are invalidated.
const RUMDL_VERSION: &str = "0.2.28+defaults3-mdx-tmplskip";

/// rumdl-proprietary stylistic rules disabled by default.
///
/// These extend beyond the standard markdownlint set (which stops at MD059) and
/// are purely stylistic — table-cell alignment (MD060), heading capitalization
/// (MD063), frontmatter key sort (MD072), heading-anchor collisions (MD080), and
/// empty sections (MD082). Per the project's defaults policy ("purely stylistic
/// rules: pick one convention or turn the rule off — never bikeshed"), they are
/// off by default. A user can re-enable any of them via an `enable` list in the
/// `[lint.markdown.rumdl]` table of `poly.toml`.
const DEFAULT_DISABLED_RULES: &[&str] = &["MD060", "MD063", "MD072", "MD080", "MD082"];

static LANGUAGES: &[Language] = &[Language::Markdown, Language::Mdx];

impl Engine for RumdlEngine {
    fn name(&self) -> &'static str {
        "rumdl"
    }

    fn languages(&self) -> &'static [Language] {
        LANGUAGES
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            lint: true,
            format: true,
            fix: true,
        }
    }

    fn version(&self) -> &str {
        RUMDL_VERSION
    }

    fn lint(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        if contains_go_template(&src.content) {
            tracing::info!(path = %src.path.display(), "skipping file with Go/Helm template syntax");
            return Ok(Vec::new());
        }
        let rumdl_cfg = build_rumdl_config(cfg, &src.language);
        let rules = filter_rules(&all_rules(&rumdl_cfg), &rumdl_cfg.global);
        let flavor = rumdl_cfg.markdown_flavor();
        let format_owned = format_owned_rules(&rules);
        rumdl_lib::lint(
            &src.content,
            &rules,
            false,
            flavor,
            Some(src.path.clone()),
            Some(&rumdl_cfg),
        )
        .map(|warnings| {
            warnings
                .iter()
                .filter(|w| !is_format_owned(w, &format_owned))
                .map(|w| map_warning(w, "rumdl"))
                .collect()
        })
        .map_err(|e| anyhow::anyhow!("rumdl lint: {e}"))
    }

    fn format(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
        if contains_go_template(&src.content) {
            tracing::info!(path = %src.path.display(), "skipping file with Go/Helm template syntax");
            return Ok(FormatOutput::Unchanged);
        }
        let rumdl_cfg = build_rumdl_config(cfg, &src.language);
        let rules = filter_rules(&all_rules(&rumdl_cfg), &rumdl_cfg.global);
        let coordinator = FixCoordinator::default();
        let mut content = src.content.to_string();
        coordinator
            .apply_fixes_iterative(&rules, &[], &mut content, &rumdl_cfg, 10, Some(&src.path))
            .map_err(|e| anyhow::anyhow!("rumdl format: {e}"))?;
        if content == *src.content {
            Ok(FormatOutput::Unchanged)
        } else {
            Ok(FormatOutput::Formatted(content))
        }
    }
}

/// Build a [`RumdlConfig`] from the resolved engine config, applying the opinionated override
/// layer (line-length 120) before user options. `language` selects rumdl's Markdown
/// flavor: [`Language::Mdx`] enables the MDX flavor (JSX + ESM aware) so `.mdx`
/// files are parsed correctly.
fn build_rumdl_config(cfg: &EngineConfig, language: &Language) -> RumdlConfig {
    let mut config = RumdlConfig::default();

    if *language == Language::Mdx {
        config.global.flavor = MarkdownFlavor::MDX;
    }

    let line_length = cfg
        .options
        .get("line_length")
        .and_then(toml::Value::as_integer)
        .map(|v| v as usize)
        .unwrap_or(cfg.globals.line_length);
    config.global.line_length = LineLength::new(line_length);

    let selection = RuleSelection::from_options(cfg);

    let user_enable = warn_and_skip_blank(
        union_codes(
            string_list(cfg, "enable"),
            selection.select.into_iter().chain(selection.extend_select),
        ),
        "rumdl",
    );
    let user_disable = warn_and_skip_blank(union_codes(string_list(cfg, "disable"), selection.ignore), "rumdl");

    let mut disable: Vec<String> = DEFAULT_DISABLED_RULES
        .iter()
        .filter(|rule| !user_enable.iter().any(|e| e.eq_ignore_ascii_case(rule)))
        .map(|rule| (*rule).to_owned())
        .collect();
    disable.extend(user_disable);

    config.global.disable = disable;
    config.global.enable = user_enable;
    config
}

/// The set of rule codes (e.g. `"MD013"`) that belong to rumdl's `Whitespace`
/// category — the formatting rules `poly fmt` owns. Built from the active rule
/// set so it tracks the tool's own categorisation rather than a hardcoded list.
fn format_owned_rules(rules: &[Box<dyn Rule>]) -> HashSet<&'static str> {
    rules
        .iter()
        .filter(|rule| rule.category() == RuleCategory::Whitespace)
        .map(|rule| rule.name())
        .collect()
}

/// Whether a warning belongs to a formatting-owned rule and must not surface in
/// `poly lint`. A warning with no rule name is never suppressed (it cannot be
/// attributed to a formatting rule).
fn is_format_owned(warning: &LintWarning, format_owned: &HashSet<&'static str>) -> bool {
    warning
        .rule_name
        .as_deref()
        .is_some_and(|name| format_owned.contains(name))
}

/// Map a rumdl [`LintWarning`] to the shared [`Diagnostic`] type.
fn map_warning(w: &LintWarning, engine: &str) -> Diagnostic {
    let severity = match w.severity {
        RumdlSeverity::Error => Severity::Error,
        RumdlSeverity::Warning => Severity::Warning,
        RumdlSeverity::Info => Severity::Info,
    };
    let fix: Vec<Edit> = w
        .fix
        .as_ref()
        .map(|f| Edit {
            start_byte: f.range.start,
            end_byte: f.range.end,
            replacement: f.replacement.clone(),
        })
        .into_iter()
        .collect();
    Diagnostic {
        engine: engine.to_owned(),
        code: w.rule_name.clone(),
        severity,
        title: w.message.clone(),
        description: None,
        url: None,
        span: Some(Span {
            start_line: w.line as u32,
            start_col: w.column as u32,
            end_line: w.end_line as u32,
            end_col: w.end_column as u32,
        }),
        fix,
        metadata: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::config::{EngineConfig, GlobalDefaults};

    fn default_cfg() -> EngineConfig {
        EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 4,
            options: toml::Table::new(),
        }
    }

    fn source(content: &str) -> SourceFile {
        SourceFile {
            path: PathBuf::from("test.md"),
            language: Language::Markdown,
            content: content.into(),
        }
    }

    fn mdx_source(content: &str) -> SourceFile {
        SourceFile {
            path: PathBuf::from("test.mdx"),
            language: Language::Mdx,
            content: content.into(),
        }
    }

    #[test]
    fn mdx_language_selects_mdx_flavor() {
        let cfg = default_cfg();
        assert_eq!(
            build_rumdl_config(&cfg, &Language::Mdx).markdown_flavor(),
            MarkdownFlavor::MDX
        );
        assert_ne!(
            build_rumdl_config(&cfg, &Language::Markdown).markdown_flavor(),
            MarkdownFlavor::MDX
        );
    }

    #[test]
    fn mdx_file_is_linted() {
        let engine = RumdlEngine;
        let src = mdx_source("#Bad Heading\n\nContent.\n");
        let diags = engine.lint(&src, &default_cfg()).expect("lint succeeded");
        let codes: Vec<_> = diags.iter().filter_map(|d| d.code.as_deref()).collect();
        assert!(codes.contains(&"MD018"), "mdx file should be linted, got {codes:?}");
    }

    #[test]
    fn helm_templated_markdown_is_skipped() {
        let engine = RumdlEngine;
        let src = source("#Bad Heading\n\n{{- if .Values.enabled }}\nContent.\n{{- end }}\n");
        assert!(
            engine.lint(&src, &default_cfg()).expect("lint succeeded").is_empty(),
            "templated markdown must be skipped by lint"
        );
        assert!(
            matches!(
                engine.format(&src, &default_cfg()).expect("format succeeded"),
                FormatOutput::Unchanged
            ),
            "templated markdown must be skipped by format"
        );
    }

    #[test]
    fn lint_returns_diagnostics_for_invalid_heading() {
        let engine = RumdlEngine;
        let src = source("#Bad Heading\n\nContent.\n");
        let cfg = default_cfg();
        let diags = engine.lint(&src, &cfg).expect("lint succeeded");
        let codes: Vec<_> = diags.iter().filter_map(|d| d.code.as_deref()).collect();
        assert!(codes.contains(&"MD018"), "expected MD018 in {codes:?}");
    }

    #[test]
    fn lint_suppresses_whitespace_category_rules() {
        let engine = RumdlEngine;
        let long = "x".repeat(200);
        let src = source(&format!("# Heading\n\n{long}\n"));
        let cfg = default_cfg();
        let diags = engine.lint(&src, &cfg).expect("lint succeeded");
        let codes: Vec<_> = diags.iter().filter_map(|d| d.code.as_deref()).collect();
        assert!(
            !codes.contains(&"MD013"),
            "MD013 (Whitespace category) must be suppressed in lint, got {codes:?}"
        );
    }

    #[test]
    fn lint_keeps_structural_rules() {
        let engine = RumdlEngine;
        let src = source("#Bad Heading\n\nContent.\n");
        let cfg = default_cfg();
        let diags = engine.lint(&src, &cfg).expect("lint succeeded");
        let codes: Vec<_> = diags.iter().filter_map(|d| d.code.as_deref()).collect();
        assert!(codes.contains(&"MD018"), "structural rule must survive, got {codes:?}");
    }

    #[test]
    fn format_removes_trailing_whitespace() {
        let engine = RumdlEngine;
        let src = source("# Heading\n\nLine with trailing spaces   \n\nContent.\n");
        let cfg = default_cfg();
        match engine.format(&src, &cfg).expect("format succeeded") {
            FormatOutput::Formatted(out) => {
                assert!(
                    !out.contains("   \n"),
                    "trailing whitespace should be removed, got:\n{out}"
                );
            }
            FormatOutput::Unchanged => panic!("expected Formatted, got Unchanged"),
        }
    }

    #[test]
    fn format_already_clean_is_unchanged() {
        let engine = RumdlEngine;
        let src = source("# Heading\n\nClean line.\n");
        let cfg = default_cfg();
        assert!(
            matches!(
                engine.format(&src, &cfg).expect("format succeeded"),
                FormatOutput::Unchanged
            ),
            "already-clean file should be Unchanged"
        );
    }
}
