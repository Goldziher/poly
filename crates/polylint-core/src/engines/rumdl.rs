//! rumdl backend — Markdown lint and auto-format.
//!
//! Wraps [`rumdl_lib`](https://crates.io/crates/rumdl) in-process — no subprocess, no system
//! dependency. Capabilities: lint (all default rules) + format (apply all auto-fixable rules
//! iteratively until convergence).
//!
//! Config layering: rumdl defaults → opinionated override (line-length 120) → user
//! `[lint.markdown.rumdl]` / `[fmt.markdown.rumdl]` table in `polylint.toml`.

use rumdl_lib::{
    config::Config as RumdlConfig,
    fix_coordinator::FixCoordinator,
    rule::{LintWarning, Severity as RumdlSeverity},
    rules::all_rules,
    types::LineLength,
};

use crate::config::EngineConfig;
use crate::engine::{
    Capabilities, Diagnostic, Edit, Engine, FormatOutput, Severity, SourceFile, Span,
};
use crate::language::Language;

/// rumdl Markdown lint + format backend.
pub struct RumdlEngine;

/// Embedded crate version so the cache key changes whenever rumdl output could change.
const RUMDL_VERSION: &str = "0.2.23";

static LANGUAGES: &[Language] = &[Language::Markdown];

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
        let rumdl_cfg = build_rumdl_config(cfg);
        let rules = all_rules(&rumdl_cfg);
        let flavor = rumdl_cfg.markdown_flavor();
        rumdl_lib::lint(
            &src.content,
            &rules,
            false,
            flavor,
            Some(src.path.clone()),
            Some(&rumdl_cfg),
        )
        .map(|warnings| warnings.iter().map(|w| map_warning(w, "rumdl")).collect())
        .map_err(|e| anyhow::anyhow!("rumdl lint: {e:?}"))
    }

    fn format(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
        let rumdl_cfg = build_rumdl_config(cfg);
        let rules = all_rules(&rumdl_cfg);
        let coordinator = FixCoordinator::default();
        let mut content = src.content.clone();
        coordinator
            .apply_fixes_iterative(&rules, &[], &mut content, &rumdl_cfg, 10, Some(&src.path))
            .map_err(|e| anyhow::anyhow!("rumdl format: {e}"))?;
        if content == src.content {
            Ok(FormatOutput::Unchanged)
        } else {
            Ok(FormatOutput::Formatted(content))
        }
    }
}

/// Build a [`RumdlConfig`] from the resolved engine config, applying the opinionated override
/// layer (line-length 120) before user options.
fn build_rumdl_config(cfg: &EngineConfig) -> RumdlConfig {
    let mut config = RumdlConfig::default();

    // Opinionated default: line-length 120; user option overrides.
    let line_length = cfg
        .options
        .get("line_length")
        .and_then(toml::Value::as_integer)
        .map(|v| v as usize)
        .unwrap_or(cfg.globals.line_length);
    config.global.line_length = LineLength::new(line_length);

    // Optional rule-override lists from polylint.toml.
    if let Some(arr) = cfg.options.get("disable").and_then(toml::Value::as_array) {
        config.global.disable = arr
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect();
    }
    if let Some(arr) = cfg.options.get("enable").and_then(toml::Value::as_array) {
        config.global.enable = arr
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect();
    }
    config
}

/// Map a rumdl [`LintWarning`] to the shared [`Diagnostic`] type.
fn map_warning(w: &LintWarning, engine: &str) -> Diagnostic {
    let severity = match w.severity {
        RumdlSeverity::Error => Severity::Error,
        RumdlSeverity::Warning => Severity::Warning,
        RumdlSeverity::Info => Severity::Info,
    };
    let fix = w.fix.as_ref().map(|f| Edit {
        start_byte: f.range.start,
        end_byte: f.range.end,
        replacement: f.replacement.clone(),
    });
    Diagnostic {
        engine: engine.to_owned(),
        code: w.rule_name.clone(),
        severity,
        message: w.message.clone(),
        span: Some(Span {
            start_line: w.line as u32,
            start_col: w.column as u32,
            end_line: w.end_line as u32,
            end_col: w.end_column as u32,
        }),
        fix,
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
            content: content.to_owned(),
        }
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
