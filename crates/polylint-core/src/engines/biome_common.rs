//! Shared helpers for the biome lint backends (`biome_graphql` and `biome_css`).
//!
//! Centralises diagnostic mapping, byte-offset → line/col conversion, and rule
//! filter construction so that each engine file is focused on its
//! language-specific parse + analyze call.

use biome_analyze::{AnalysisFilter, AnalyzerDiagnostic, RuleCategoriesBuilder, RuleFilter};
use biome_diagnostics::Diagnostic as BiomeDiag;

use crate::config::EngineConfig;
use crate::engine::{Diagnostic, Severity, Span};
use crate::engines::rule_config::RuleSelection;

// ── offset → (line, col) ─────────────────────────────────────────────────────

/// Convert a byte offset to 1-based `(line, col)` in a UTF-8 source string.
///
/// Mirrors the identical helper in `oxc.rs`; centralised here so both biome
/// engine backends share one copy rather than duplicating the logic.
pub(crate) fn offset_to_line_col(src: &str, offset: usize) -> (u32, u32) {
    let safe_offset = offset.min(src.len());
    let mut line: u32 = 1;
    let mut col: u32 = 1;
    for (i, ch) in src.char_indices() {
        if i >= safe_offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// Convert a biome [`biome_rowan::TextRange`] (byte offsets, u32-backed) to a
/// polylint 1-based [`Span`].
pub(crate) fn text_range_to_span(src: &str, range: biome_rowan::TextRange) -> Span {
    let start = u32::from(range.start()) as usize;
    let end = u32::from(range.end()) as usize;
    let (start_line, start_col) = offset_to_line_col(src, start);
    let (end_line, end_col) = offset_to_line_col(src, end);
    Span {
        start_line,
        start_col,
        end_line,
        end_col,
    }
}

// ── diagnostic mapping ────────────────────────────────────────────────────────

/// Capture a biome `Diagnostic::description` as a plain `String`.
///
/// `AnalyzerDiagnostic::description` calls `Debug::fmt` on the inner
/// `MessageAndDescription`.  That `Debug` impl delegates to `Display`, which
/// calls `f.write_str(&self.description)` — writing the pre-rendered
/// plain-text string.  The result is the human-readable rule message with no
/// markup or escape characters.
fn diag_title(diag: &dyn BiomeDiag) -> String {
    struct DescFmt<'a>(&'a dyn BiomeDiag);
    impl std::fmt::Display for DescFmt<'_> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            self.0.description(f)
        }
    }
    format!("{}", DescFmt(diag))
}

/// Map one [`AnalyzerDiagnostic`] emitted by biome to a polylint [`Diagnostic`].
///
/// * **code**: `category().name()` — e.g. `"lint/correctness/noUnknownProperty"`.
///   This is the stable string users target in `[rules.<code>]` in `polylint.toml`.
/// * **severity**: biome `Severity` → polylint `Severity`.  Per-rule severity
///   overrides are handled generically by the runner's `build_severity_remap`
///   (keyed on `Diagnostic.code`) — engines do not plumb them through
///   `AnalyzerRules`.
/// * **title**: plain description from `AnalyzerDiagnostic::description()`.
/// * **span**: biome `Location::span` (`TextRange`) converted to 1-based line/col.
/// * **fix**: empty — lint-only for v1.  `capabilities().fix = false`.
pub(crate) fn map_biome_diag(diag: &AnalyzerDiagnostic, content: &str, engine_name: &'static str) -> Diagnostic {
    let code = diag.category().map(|c| c.name().to_owned());

    let severity = match diag.severity() {
        biome_diagnostics::Severity::Error | biome_diagnostics::Severity::Fatal => Severity::Error,
        biome_diagnostics::Severity::Warning => Severity::Warning,
        biome_diagnostics::Severity::Information => Severity::Info,
        biome_diagnostics::Severity::Hint => Severity::Hint,
    };

    let title = diag_title(diag);

    let span = diag.location().span.map(|range| text_range_to_span(content, range));

    Diagnostic {
        engine: engine_name.to_owned(),
        code,
        severity,
        title,
        description: None,
        span,
        url: None,
        fix: vec![],
        metadata: Default::default(),
    }
}

// ── rule filter construction ──────────────────────────────────────────────────

/// Build `(enabled_strs, disabled_strs)` from a `[lint.<lang>.biome]` engine
/// config, falling back to `default_groups` when the user provides no `select`.
///
/// The caller builds `Vec<RuleFilter<'_>>` from the returned strings and keeps
/// both `Vec<String>`s alive through the `analyze()` call, so the borrowed
/// `&str` pointers inside `AnalysisFilter` remain valid.
pub(crate) fn rule_filter_strings(cfg: &EngineConfig, default_groups: &[&str]) -> (Vec<String>, Vec<String>) {
    let selection = RuleSelection::from_options(cfg);

    let enabled: Vec<String> = if selection.select.is_empty() {
        // No explicit select: opinionated defaults + any extend_select additions.
        let mut v: Vec<String> = default_groups.iter().map(|&s| s.to_owned()).collect();
        v.extend_from_slice(&selection.extend_select);
        v
    } else {
        // Explicit select: replace defaults, then extend.
        let mut v = selection.select.clone();
        v.extend_from_slice(&selection.extend_select);
        v
    };

    (enabled, selection.ignore.clone())
}

/// Convert a group-or-rule string to a [`RuleFilter`].
///
/// `"correctness"` → `RuleFilter::Group("correctness")`.
/// `"correctness/noUnknownProperty"` → `RuleFilter::Rule("correctness", "noUnknownProperty")`.
pub(crate) fn str_to_rule_filter(s: &str) -> RuleFilter<'_> {
    if let Some((group, rule)) = s.split_once('/') {
        RuleFilter::Rule(group, rule)
    } else {
        RuleFilter::Group(s)
    }
}

/// Build an [`AnalysisFilter`] restricted to lint-only categories.
///
/// * `enabled_filters` — the rules/groups to enable.
/// * `disabled_filters` — the rules/groups to disable even if enabled.
///
/// The filter borrows from both slices, so both slices **must** outlive the
/// `analyze()` call.
pub(crate) fn build_lint_filter<'a>(
    enabled_filters: &'a [RuleFilter<'a>],
    disabled_filters: &'a [RuleFilter<'a>],
) -> AnalysisFilter<'a> {
    AnalysisFilter {
        enabled_rules: Some(enabled_filters),
        disabled_rules: disabled_filters,
        categories: RuleCategoriesBuilder::default().with_lint().build(),
        range: None,
    }
}
