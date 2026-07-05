//! Rule-test runner for custom ast-grep rules.
//!
//! Each rule may ship a companion `<name>-test.yml` file (ast-grep's
//! convention) holding snippets that must — or must not — trigger the rule:
//!
//! ```yaml
//! id: no-print
//! valid:
//!   - logging.info("hi")
//! invalid:
//!   - print("hi")
//! ```
//!
//! [`run_tests`] loads the rules and their test files from the given
//! directories and checks every snippet: `valid` snippets must NOT match the
//! rule, `invalid` snippets MUST match. This powers `poly rules test`.

use std::collections::{HashMap, HashSet};

use anyhow::Context;
use ast_grep_config::{CombinedScan, RuleConfig};
use ast_grep_core::tree_sitter::LanguageExt;
use serde::Deserialize;

use super::language::TslpLanguage;
use super::rules::{collect_test_paths, load_flat};

/// One `<name>-test.yml` file: snippets that assert a rule's behaviour.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleTest {
    /// The `id` of the rule under test.
    pub id: String,
    /// Snippets that must NOT trigger the rule.
    #[serde(default)]
    pub valid: Vec<String>,
    /// Snippets that MUST trigger the rule.
    #[serde(default)]
    pub invalid: Vec<String>,
}

/// Which side of the test a snippet came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseKind {
    /// The snippet must produce no match.
    Valid,
    /// The snippet must produce a match.
    Invalid,
}

/// The result of checking one snippet against one rule.
#[derive(Debug)]
pub struct CaseOutcome {
    /// The rule id this snippet was checked against.
    pub rule_id: String,
    /// Valid or invalid expectation.
    pub kind: CaseKind,
    /// Index of the snippet within its `valid` / `invalid` list.
    pub index: usize,
    /// Whether the snippet met its expectation.
    pub passed: bool,
}

/// Aggregate outcome of a `poly rules test` run.
#[derive(Debug, Default)]
pub struct TestReport {
    /// Total rules discovered under the searched dirs.
    pub total_rules: usize,
    /// Per-snippet outcomes.
    pub outcomes: Vec<CaseOutcome>,
    /// Test files whose `id` matched no loaded rule.
    pub missing_rule_ids: Vec<String>,
    /// Rules that have no `*-test.yml` file at all.
    pub untested_rule_ids: Vec<String>,
}

impl TestReport {
    /// Count of snippets that met their expectation.
    pub fn passed(&self) -> usize {
        self.outcomes.iter().filter(|o| o.passed).count()
    }

    /// Count of snippets that failed their expectation.
    pub fn failed(&self) -> usize {
        self.outcomes.iter().filter(|o| !o.passed).count()
    }

    /// A run is successful when every snippet passed and every test names a
    /// real rule. Untested rules are a warning, not a failure.
    pub fn is_ok(&self) -> bool {
        self.failed() == 0 && self.missing_rule_ids.is_empty()
    }
}

/// Does `rule` match anywhere in `code`?
///
/// Parses `code` with the rule's own (TSLP-backed) language and runs a
/// single-rule scan. `separate_fix = false` routes every hit — fixable or not —
/// into `matches`, so a non-empty `matches` means the rule fired.
pub fn rule_matches(rule: &RuleConfig<TslpLanguage>, code: &str) -> bool {
    let root = rule.language.ast_grep(code);
    let scan = CombinedScan::new(vec![rule]);
    !scan.scan(&root, false).matches.is_empty()
}

/// Check every snippet in `test` against `rule`.
pub fn verify(test: &RuleTest, rule: &RuleConfig<TslpLanguage>) -> Vec<CaseOutcome> {
    let mut outcomes = Vec::with_capacity(test.valid.len() + test.invalid.len());
    for (index, code) in test.valid.iter().enumerate() {
        outcomes.push(CaseOutcome {
            rule_id: test.id.clone(),
            kind: CaseKind::Valid,
            index,
            passed: !rule_matches(rule, code),
        });
    }
    for (index, code) in test.invalid.iter().enumerate() {
        outcomes.push(CaseOutcome {
            rule_id: test.id.clone(),
            kind: CaseKind::Invalid,
            index,
            passed: rule_matches(rule, code),
        });
    }
    outcomes
}

/// Load rules and their `*-test.yml` files from `dirs`, then verify every
/// snippet. Returns a [`TestReport`]; see [`TestReport::is_ok`] for pass/fail.
pub fn run_tests(dirs: &[String]) -> anyhow::Result<TestReport> {
    let rules = load_flat(dirs)?;
    let by_id: HashMap<&str, &RuleConfig<TslpLanguage>> = rules.iter().map(|r| (r.id.as_str(), r)).collect();

    let mut report = TestReport {
        total_rules: rules.len(),
        ..TestReport::default()
    };
    let mut tested: HashSet<String> = HashSet::new();

    for path in collect_test_paths(dirs) {
        let yaml = std::fs::read_to_string(&path).with_context(|| format!("reading test file {}", path.display()))?;
        let test: RuleTest =
            ast_grep_config::from_str(&yaml).with_context(|| format!("parsing rule test {}", path.display()))?;

        match by_id.get(test.id.as_str()) {
            Some(rule) => {
                tested.insert(test.id.clone());
                report.outcomes.extend(verify(&test, rule));
            }
            None => report.missing_rule_ids.push(test.id.clone()),
        }
    }

    report.untested_rule_ids = rules
        .iter()
        .map(|r| r.id.clone())
        .filter(|id| !tested.contains(id))
        .collect();
    report.untested_rule_ids.sort();
    report.untested_rule_ids.dedup();

    Ok(report)
}
