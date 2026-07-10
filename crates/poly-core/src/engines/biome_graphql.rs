//! Biome GraphQL lint backend.
//!
//! Wraps `biome_graphql_analyze::analyze` to run biome's GraphQL lint rules
//! in-process. Capabilities: **lint-only**; the `GraphQlEngine` holds the
//! format slot and is unaffected.
//!
//! # Registry coexistence
//!
//! Both `GraphQlEngine` (parse-error lint + formatting) and `BiomeGraphqlEngine`
//! (rule-based lint) are registered for `Language::GraphQl`.  The runner
//! dispatches by capability: format runs use `GraphQlEngine`; lint runs collect
//! diagnostics from both. The two engines emit complementary diagnostics —
//! parse errors from `GraphQlEngine` and rule violations from this engine —
//! so there is no duplication.
//!
//! # Config table
//!
//! `[lint.graphql.biome]` in `poly.toml`.  Supports the poly uniform
//! rule-selection surface (`select`, `extend_select`, `ignore`,
//! `[rules.<code>]`).
//!
//! # Opinionated defaults
//!
//! Groups `correctness` and `suspicious` are ON by default.
//! Group `nursery` is OFF (unstable rules).
//! All other groups (`style`) are opt-in via `extend_select`.
//!
//! # Engine name
//!
//! `"biome"` — matches the `[lint.graphql.biome]` config key and the
//! `Diagnostic.engine` field surfaced in reports.

use biome_analyze::{AnalyzerOptions, ControlFlow, Never};
use biome_graphql_parser::parse_graphql;

use crate::config::EngineConfig;
use crate::engine::{Capabilities, Diagnostic, Engine, SourceFile};
use crate::engines::biome_common::{build_lint_filter, map_biome_diag, rule_filter_strings, str_to_rule_filter};
use crate::language::Language;

/// Biome GraphQL lint engine.
///
/// Runs biome's `correctness` + `suspicious` GraphQL rule groups by default.
/// Nursery rules are off; opt in with `extend_select = ["nursery"]` in
/// `[lint.graphql.biome]`.
pub struct BiomeGraphqlEngine;

static LANGUAGES: &[Language] = &[Language::GraphQl];

/// Opinionated default rule groups: real correctness bugs + suspicious patterns.
/// Nursery is excluded (unstable). Style is opt-in.
const DEFAULT_GROUPS: &[&str] = &["correctness", "suspicious"];

/// Cache-key version string.  Embeds the pinned biome rev so the blake3 cache
/// is invalidated whenever the rev changes.  Bump the `+lint-vN` suffix when
/// the diagnostic mapping logic changes output for identical input.
const VERSION: &str = "biome_graphql_analyze+rev:93d8e53+lint-v1";

impl Engine for BiomeGraphqlEngine {
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
            fix: false,
        }
    }

    fn version(&self) -> &str {
        VERSION
    }

    fn lint(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        let parsed = parse_graphql(&src.content);
        let root = parsed.tree();

        let (enabled_strs, disabled_strs) = rule_filter_strings(cfg, DEFAULT_GROUPS);
        let enabled_filters: Vec<_> = enabled_strs.iter().map(|s| str_to_rule_filter(s)).collect();
        let disabled_filters: Vec<_> = disabled_strs.iter().map(|s| str_to_rule_filter(s)).collect();
        let filter = build_lint_filter(&enabled_filters, &disabled_filters);

        let options = AnalyzerOptions::default();
        let mut out: Vec<Diagnostic> = Vec::new();

        let (_, _parse_errors) = biome_graphql_analyze::analyze(&root, filter, &options, |signal| {
            if let Some(diag) = signal.diagnostic() {
                out.push(map_biome_diag(&diag, &src.content, "biome"));
            }
            ControlFlow::<Never>::Continue(())
        });

        Ok(out)
    }
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
        let engine = BiomeGraphqlEngine;
        assert_eq!(engine.name(), "biome");
        assert!(engine.capabilities().lint);
        assert!(!engine.capabilities().format);
        assert!(!engine.capabilities().fix);
        assert!(engine.languages().contains(&Language::GraphQl));
    }

    #[test]
    fn valid_graphql_produces_no_diagnostics() {
        let engine = BiomeGraphqlEngine;
        let src = crate::engine::SourceFile {
            path: "test.graphql".into(),
            language: Language::GraphQl,
            content: "query GetUser { user { id name } }\n".into(),
        };
        let diags = engine.lint(&src, &default_cfg()).unwrap();
        assert!(diags.is_empty(), "expected no diagnostics; got: {diags:#?}");
    }
}
