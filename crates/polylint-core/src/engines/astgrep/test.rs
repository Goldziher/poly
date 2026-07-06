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
//! An `invalid` entry may also assert the rule's **autofix output** by giving a
//! table with `code` (the input) and `fixed` (the expected rewrite) instead of a
//! bare snippet string:
//!
//! ```yaml
//! id: use-is-none
//! invalid:
//!   - x == None                 # must match; fix output unchecked
//!   - code: x == None           # must match AND autofix to `x is None`
//!     fixed: x is None
//! ```
//!
//! [`run_tests`] loads the rules and their test files from the given
//! directories and checks every snippet: `valid` snippets must NOT match the
//! rule, `invalid` snippets MUST match, and any `fixed:` expectation must equal
//! the rule's applied autofix. This powers `poly rules test`.

use std::collections::{HashMap, HashSet};

use anyhow::Context;
use ast_grep_config::{CombinedScan, RuleConfig};
use ast_grep_core::tree_sitter::LanguageExt;
use serde::Deserialize;

use super::language::TslpLanguage;
use super::rules::{collect_test_paths, load_flat};
use crate::engine::Edit;

/// One `<name>-test.yml` file: snippets that assert a rule's behaviour.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleTest {
    /// The `id` of the rule under test.
    pub id: String,
    /// Snippets that must NOT trigger the rule.
    #[serde(default)]
    pub valid: Vec<String>,
    /// Snippets that MUST trigger the rule (optionally asserting fix output).
    #[serde(default)]
    pub invalid: Vec<InvalidCase>,
}

/// An `invalid` test case: a snippet that must trigger the rule.
///
/// Either a bare snippet string (match-only) or a table carrying an expected
/// autofix result. Deserialized untagged, so both YAML shapes are accepted:
/// `- print("x")` and `- { code: print("x"), fixed: log("x") }`.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum InvalidCase {
    /// A snippet that must match; its fix output is not asserted.
    Snippet(String),
    /// A snippet that must match, whose applied autofix must equal `fixed`.
    WithFix {
        /// The source snippet fed to the rule.
        code: String,
        /// The exact source expected after applying the rule's autofix.
        fixed: String,
    },
}

impl InvalidCase {
    /// The source snippet under test.
    fn code(&self) -> &str {
        match self {
            InvalidCase::Snippet(code) => code,
            InvalidCase::WithFix { code, .. } => code,
        }
    }

    /// The asserted autofix output, if this case carries a `fixed:` expectation.
    fn expected_fix(&self) -> Option<&str> {
        match self {
            InvalidCase::Snippet(_) => None,
            InvalidCase::WithFix { fixed, .. } => Some(fixed),
        }
    }
}

/// Which side of the test a snippet came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseKind {
    /// The snippet must produce no match.
    Valid,
    /// The snippet must produce a match.
    Invalid,
    /// The snippet's applied autofix must equal the asserted `fixed:` output.
    Fixed,
}

/// The result of checking one snippet against one rule.
#[derive(Debug)]
pub struct CaseOutcome {
    /// The rule id this snippet was checked against.
    pub rule_id: String,
    /// Valid, invalid, or fix-output expectation.
    pub kind: CaseKind,
    /// Index of the snippet within its `valid` / `invalid` list.
    pub index: usize,
    /// Whether the snippet met its expectation.
    pub passed: bool,
    /// Human-readable failure detail (e.g. the got/want fix mismatch). `None`
    /// when the case passed or carries no extra context.
    pub detail: Option<String>,
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

/// Apply `rule`'s autofix to `code` and return the rewritten source.
///
/// Scans with `separate_fix = true` so fixable hits land in `diffs`, builds the
/// same byte-range [`Edit`]s the runner applies (via [`super::map::fix_edits`]),
/// and replaces them rightmost-first so earlier offsets stay valid. Returns
/// `None` when the rule declares no `fix` or nothing matched — the caller
/// reports that as a fix mismatch.
pub fn apply_rule_fix(rule: &RuleConfig<TslpLanguage>, code: &str) -> Option<String> {
    let root = rule.language.ast_grep(code);
    let scan = CombinedScan::new(vec![rule]);
    let mut edits: Vec<Edit> = scan
        .scan(&root, true)
        .diffs
        .iter()
        .flat_map(|(matched_rule, node_match)| super::map::fix_edits(matched_rule, node_match))
        .collect();
    if edits.is_empty() {
        return None;
    }
    edits.sort_by_key(|e| std::cmp::Reverse(e.start_byte));
    let mut out = code.to_string();
    for edit in edits {
        out.replace_range(edit.start_byte..edit.end_byte, &edit.replacement);
    }
    Some(out)
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
            detail: None,
        });
    }
    for (index, case) in test.invalid.iter().enumerate() {
        outcomes.push(CaseOutcome {
            rule_id: test.id.clone(),
            kind: CaseKind::Invalid,
            index,
            passed: rule_matches(rule, case.code()),
            detail: None,
        });
        // An `invalid` case may additionally assert its autofix output.
        if let Some(expected) = case.expected_fix() {
            let got = apply_rule_fix(rule, case.code());
            let passed = got.as_deref() == Some(expected);
            let detail = (!passed).then(|| match &got {
                Some(actual) => format!("fix output `{expected}` but got `{actual}`"),
                None => format!("fix output `{expected}` but the rule produced no fix"),
            });
            outcomes.push(CaseOutcome {
                rule_id: test.id.clone(),
                kind: CaseKind::Fixed,
                index,
                passed,
                detail,
            });
        }
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
