//! The rule inventory as it crosses the component boundary.
//!
//! [`rules_report`] is the one place the listing and the test run become a
//! [`WireRulesReport`]. The bundled build converts it in-process and the astgrep component
//! answers `Request::Rules` with it, so both flavours report the same rules.

use poly_component_protocol::{WireRule, WireRuleTestOutcome, WireRuleTests, WireRulesReport};
use poly_engine_api::config::EngineConfig;

use crate::test::{CaseKind, run_tests};
use crate::{builtin_pack_enabled, list_rules};

/// List the rules `cfg` resolves to over `dirs` and, when `test` is set, run their test snippets.
///
/// # Errors
///
/// When a rule directory cannot be read or a rule or test file does not parse.
pub fn rules_report(dirs: &[String], cfg: &EngineConfig, test: bool) -> anyhow::Result<WireRulesReport> {
    let rules = list_rules(dirs, cfg)?
        .into_iter()
        .map(|rule| WireRule {
            id: rule.id,
            language: rule.language,
            source: rule.source.as_str().to_owned(),
            default_severity: rule.default_severity.to_owned(),
            severity: rule.severity.to_owned(),
            enabled: rule.enabled,
        })
        .collect();
    let tests = if test {
        let report = run_tests(dirs)?;
        Some(WireRuleTests {
            total_rules: report.total_rules as u64,
            outcomes: report
                .outcomes
                .iter()
                .map(|outcome| WireRuleTestOutcome {
                    rule_id: outcome.rule_id.clone(),
                    kind: match outcome.kind {
                        CaseKind::Valid => "valid",
                        CaseKind::Invalid => "invalid",
                        CaseKind::Fixed => "fixed",
                    }
                    .to_owned(),
                    index: outcome.index as u64,
                    passed: outcome.passed,
                    detail: outcome.detail.clone(),
                })
                .collect(),
            missing_rule_ids: report.missing_rule_ids,
            untested_rule_ids: report.untested_rule_ids,
        })
    } else {
        None
    };
    Ok(WireRulesReport {
        builtin_pack_enabled: builtin_pack_enabled(cfg),
        rules,
        tests,
    })
}

#[cfg(test)]
mod tests {
    use poly_engine_api::config::GlobalDefaults;

    use super::*;

    fn config() -> EngineConfig {
        EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 4,
            options: toml::Table::new(),
        }
    }

    #[test]
    fn lists_the_builtin_pack_and_skips_tests_unless_asked() {
        let report = rules_report(&[], &config(), false).unwrap();
        assert!(report.builtin_pack_enabled);
        assert!(
            report
                .rules
                .iter()
                .any(|rule| rule.id == "swallowed-error" && rule.source == "builtin")
        );
        assert!(report.tests.is_none());
    }

    #[test]
    fn a_requested_test_run_is_reported_even_with_no_user_rules() {
        let report = rules_report(&[], &config(), true).unwrap();
        let tests = report.tests.expect("tests requested");
        assert_eq!(tests.total_rules, 0);
        assert!(tests.outcomes.is_empty());
    }
}
