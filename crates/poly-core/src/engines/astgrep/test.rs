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
use std::path::{Path, PathBuf};

use anyhow::Context;
use ast_grep_config::{CombinedScan, RuleConfig};
use ast_grep_core::tree_sitter::LanguageExt;
use serde::Deserialize;

use super::language::TslpLanguage;
use super::rules::{collect_test_paths, load_flat_with_paths};
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
        if !out.is_char_boundary(edit.start_byte) || !out.is_char_boundary(edit.end_byte) {
            continue;
        }
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
        let matched = rule_matches(rule, case.code());
        outcomes.push(CaseOutcome {
            rule_id: test.id.clone(),
            kind: CaseKind::Invalid,
            index,
            passed: matched,
            detail: None,
        });
        if let (true, Some(expected)) = (matched, case.expected_fix()) {
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

/// Identify a rule for test-file correlation by its **directory** plus `id`,
/// not `id` alone.
///
/// poly's built-in pack deliberately ships the same id (`todo-marker`) once
/// per language, each rule and its companion `<id>-test.yml` living in that
/// language's own directory (`builtin/rust/todo-marker.yml` +
/// `builtin/rust/todo-marker-test.yml`, `builtin/python/todo-marker.yml` +
/// `builtin/python/todo-marker-test.yml`). Keying a lookup on `id` alone lets
/// the second directory's rule/test pair shadow the first: `poly rules test
/// builtin/` reported `373 passed, 3 failed` this way, while testing each
/// language directory alone was green. Keying on `(directory, id)` instead
/// resolves each test file against the rule(s) that actually live beside it.
fn rule_key(path: &Path, id: &str) -> (PathBuf, String) {
    (path.parent().map(Path::to_path_buf).unwrap_or_default(), id.to_string())
}

/// Load rules and their `*-test.yml` files from `dirs`, then verify every
/// snippet. Returns a [`TestReport`]; see [`TestReport::is_ok`] for pass/fail.
pub fn run_tests(dirs: &[String]) -> anyhow::Result<TestReport> {
    let rules = load_flat_with_paths(dirs)?;
    let by_key: HashMap<(PathBuf, String), &RuleConfig<TslpLanguage>> = rules
        .iter()
        .map(|(path, rule)| (rule_key(path, &rule.id), rule))
        .collect();

    let mut report = TestReport {
        total_rules: rules.len(),
        ..TestReport::default()
    };
    let mut tested: HashSet<(PathBuf, String)> = HashSet::new();

    for path in collect_test_paths(dirs) {
        let yaml = std::fs::read_to_string(&path).with_context(|| format!("reading test file {}", path.display()))?;
        let test: RuleTest =
            ast_grep_config::from_str(&yaml).with_context(|| format!("parsing rule test {}", path.display()))?;

        let key = rule_key(&path, &test.id);
        match by_key.get(&key) {
            Some(rule) => {
                tested.insert(key);
                report.outcomes.extend(verify(&test, rule));
            }
            None => report.missing_rule_ids.push(test.id.clone()),
        }
    }

    let mut untested_keys: Vec<(PathBuf, String)> = rules
        .iter()
        .map(|(path, rule)| rule_key(path, &rule.id))
        .filter(|key| !tested.contains(key))
        .collect();
    untested_keys.sort();
    untested_keys.dedup();
    report.untested_rule_ids = untested_keys.into_iter().map(|(_, id)| id).collect();

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two rules sharing an id across different language directories — poly's
    /// built-in pack's actual shape for `todo-marker` (Rust + Python) — must
    /// both be tested independently, neither shadowing the other. Reverting
    /// `rule_key` to key on `id` alone reproduces the historical defect: one
    /// directory's rule/test pair silently absorbs the other's, and this test
    /// fails (either a wrong-language snippet unexpectedly passes, or
    /// `total_rules`/outcome counts drop from 4 to 2).
    #[test]
    fn same_id_rules_in_different_language_dirs_are_both_tested() {
        let root = tempfile::tempdir().unwrap();
        let rust_dir = root.path().join("rust");
        let python_dir = root.path().join("python");
        std::fs::create_dir_all(&rust_dir).unwrap();
        std::fs::create_dir_all(&python_dir).unwrap();

        std::fs::write(
            rust_dir.join("todo-marker.yml"),
            "id: todo-marker\nlanguage: rust\nseverity: off\nmessage: rust todo\nrule:\n  kind: line_comment\n  regex: RUST_MARKER\n",
        )
        .unwrap();
        std::fs::write(
            rust_dir.join("todo-marker-test.yml"),
            "id: todo-marker\ninvalid:\n  - \"// RUST_MARKER\\nfn f() {}\"\nvalid:\n  - \"// PYTHON_MARKER\\nfn f() {}\"\n",
        )
        .unwrap();

        std::fs::write(
            python_dir.join("todo-marker.yml"),
            "id: todo-marker\nlanguage: python\nseverity: off\nmessage: python todo\nrule:\n  kind: comment\n  regex: PYTHON_MARKER\n",
        )
        .unwrap();
        std::fs::write(
            python_dir.join("todo-marker-test.yml"),
            "id: todo-marker\ninvalid:\n  - \"# PYTHON_MARKER\\ndef f():\\n    pass\"\nvalid:\n  - \"# RUST_MARKER\\ndef f():\\n    pass\"\n",
        )
        .unwrap();

        let dirs = vec![root.path().to_string_lossy().into_owned()];
        let report = run_tests(&dirs).unwrap();

        assert_eq!(report.total_rules, 2, "expected both rules to be discovered");
        assert!(
            report.missing_rule_ids.is_empty(),
            "neither test file should be orphaned: {:?}",
            report.missing_rule_ids
        );
        assert!(
            report.untested_rule_ids.is_empty(),
            "neither rule should be reported untested: {:?}",
            report.untested_rule_ids
        );
        // Each test file has one valid + one invalid case: 4 outcomes total.
        // If the id-collision bug were still present, one directory's rule
        // would win the lookup for both test files, and the "wrong" rule's
        // pattern would make one snippet's expectation fail (its `valid`
        // snippet actually matches the other language's rule, or vice versa).
        assert_eq!(report.outcomes.len(), 4, "expected 2 outcomes per rule; got {report:?}");
        assert_eq!(
            report.failed(),
            0,
            "each rule must be verified against its own snippets, not the other language's: {:?}",
            report.outcomes.iter().filter(|o| !o.passed).collect::<Vec<_>>()
        );
    }
}
