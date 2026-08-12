//! Doctor check: `[discovery] exclude` rules that reach further than the person
//! who wrote them meant.
//!
//! `exclude` globs are gitignore-style, so a pattern naming no parent directory
//! matches **at any depth**. `exclude = ["e2e/**"]` reads like "the `e2e`
//! directory at the top of the repo" and is one of the most natural things to
//! write — but `e2e` is also an ordinary Java/Kotlin package name, and a
//! consumer spent months with `src/test/java/io/xberg/crawlberg/e2e` outside
//! their formatting gate. Nothing failed: every run reported a clean pass over
//! the two files it could still see.
//!
//! The tell is depth. A rule that prunes directories at one depth is doing one
//! thing; a rule that prunes at two or more is matching by name across a tree,
//! which is almost never what a hand-written exclude intends. Discovery already
//! attributes each pruned directory to the rule that pruned it, so this check is
//! a walk plus a comparison — and it names the directories, because the value is
//! entirely in showing the author the tree they did not know they were hiding.

use std::path::{Path, PathBuf};

use poly_core::discover::discover_reporting;
use poly_core::{Config, ConfigSet};
use serde::Serialize;

use super::report::{Finding, Severity};

/// Distinct depths a rule must prune directories at before it is reported.
///
/// Two is the smallest number that cannot be explained by the rule doing its
/// job: one depth is a rule matching where it was pointed.
const BROAD_DEPTH_THRESHOLD: usize = 2;

/// How many matched directories a single finding names before it stops.
///
/// The finding has to fit on a terminal line or two; three is enough to show the
/// intended directory alongside the unintended ones.
const MAX_NAMED_DIRECTORIES: usize = 3;

/// One exclude rule that pruned directories at more than one depth.
#[derive(Debug, Clone, Serialize)]
pub struct BroadRule {
    /// The glob exactly as written in config.
    pub pattern: String,
    /// How many distinct depths it pruned directories at.
    pub depths: usize,
    /// How many directories it pruned in total.
    pub directories: usize,
    /// The directories it pruned, one per depth, relative to [`ExcludeReport::root`].
    pub matched: Vec<PathBuf>,
}

impl BroadRule {
    /// The finding this rule produces: a warning, because the exclude is doing
    /// what it was written to do — it is the author's intent, not poly's
    /// behaviour, that is wrong.
    fn finding(&self) -> Finding {
        let named: Vec<String> = self
            .matched
            .iter()
            .take(MAX_NAMED_DIRECTORIES)
            .map(|path| path.display().to_string())
            .collect();
        let rest = self.matched.len().saturating_sub(named.len());
        let more = if rest > 0 {
            format!(", and {rest} more")
        } else {
            String::new()
        };
        Finding {
            severity: Severity::Warning,
            summary: format!(
                "exclude rule `{}` prunes {} director(ies) at {} depths, so it is hiding more than the one \
                 directory it names: {}{more}",
                self.pattern,
                self.directories,
                self.depths,
                named.join(", ")
            ),
            remedy: Some(remedy_for(&self.pattern)),
        }
    }
}

/// The concrete fix: anchor the glob to the directory of the config that
/// declared it with a leading `/`, which is the one thing that stops a
/// gitignore-style pattern from matching by name at every depth.
fn remedy_for(pattern: &str) -> String {
    let anchored = format!("/{}", pattern.trim_start_matches('/'));
    format!(
        "anchor it to the config directory: `exclude = [\"{anchored}\"]` — an unanchored glob matches a \
         directory of that name at any depth"
    )
}

/// What the exclude scan found. Present in the JSON report so CI can assert on
/// it; the human report surfaces it through [`Finding`]s only.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ExcludeReport {
    /// The directory the scan walked — the directory of the config in effect.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root: Option<PathBuf>,
    /// Rules that pruned directories at more than one depth.
    pub broad_rules: Vec<BroadRule>,
    /// Why the scan could not run, when it could not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ExcludeReport {
    /// A report for a run with nothing to scan — no config, or a config with no
    /// `[discovery] exclude` at all.
    fn skipped() -> Self {
        Self::default()
    }
}

/// Walk `root` with the config's exclude set and report every rule that pruned
/// directories at more than one depth.
///
/// This is a full discovery walk, which no other `poly doctor` check needs. It
/// is affordable here because the walk prunes exactly what a real run prunes
/// (vendored trees, `target/`, and the excludes themselves), and because the
/// question cannot be answered without it: whether a glob is too broad is a
/// property of the repository, not of the glob.
pub fn scan(root: Option<&Path>, config: &Config) -> ExcludeReport {
    let Some(root) = root else {
        return ExcludeReport::skipped();
    };
    if config.exclude.is_empty() {
        return ExcludeReport::skipped();
    }
    let configs = match build_config_set(root, config) {
        Ok(configs) => configs,
        Err(error) => {
            return ExcludeReport {
                root: Some(root.to_path_buf()),
                broad_rules: Vec::new(),
                error: Some(format!("{error:#}")),
            };
        }
    };
    let (_, discovery) = discover_reporting(&[root.to_path_buf()], &configs, &[], false);
    let broad_rules = discovery
        .rules
        .iter()
        .filter(|rule| rule.distinct_directory_depths() >= BROAD_DEPTH_THRESHOLD)
        .map(|rule| BroadRule {
            pattern: rule.pattern.clone(),
            depths: rule.distinct_directory_depths(),
            directories: rule.directories,
            matched: rule
                .directory_samples
                .iter()
                .map(|sample| sample.path.strip_prefix(root).unwrap_or(&sample.path).to_path_buf())
                .collect(),
        })
        .collect();
    ExcludeReport {
        root: Some(root.to_path_buf()),
        broad_rules,
        error: None,
    }
}

/// Build the same hierarchical config set a real run builds, so a nested
/// `poly.toml`'s excludes are diagnosed alongside the root config's.
fn build_config_set(root: &Path, config: &Config) -> anyhow::Result<ConfigSet> {
    let resolver = crate::config_sources::resolver()?;
    ConfigSet::build_with(&[root.to_path_buf()], config.clone(), &resolver)
}

/// Turn the scan into findings.
pub fn diagnose(report: &ExcludeReport, findings: &mut Vec<Finding>) {
    if let Some(error) = &report.error {
        // Say the check did not run rather than letting silence read as a pass —
        // an unchecked exclude set is the whole failure mode this check exists for.
        findings.push(Finding {
            severity: Severity::Warning,
            summary: format!("the exclude rules could not be checked: {error}"),
            remedy: None,
        });
    }
    for rule in &report.broad_rules {
        findings.push(rule.finding());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn broad_rule() -> BroadRule {
        BroadRule {
            pattern: "e2e/**".to_string(),
            depths: 2,
            directories: 2,
            matched: vec![
                PathBuf::from("e2e"),
                PathBuf::from("test_apps/java/src/test/java/io/xberg/e2e"),
            ],
        }
    }

    #[test]
    fn a_broad_rule_names_the_pattern_the_depth_count_and_the_directories() {
        let finding = broad_rule().finding();
        assert_eq!(finding.severity, Severity::Warning);
        assert!(finding.summary.contains("`e2e/**`"), "{}", finding.summary);
        assert!(finding.summary.contains("2 depths"), "{}", finding.summary);
        assert!(
            finding.summary.contains("test_apps/java/src/test/java/io/xberg/e2e"),
            "the unintended directory is named, not merely counted: {}",
            finding.summary
        );
    }

    #[test]
    fn the_remedy_is_the_anchored_pattern() {
        let remedy = broad_rule().finding().remedy.expect("a broad rule has a remedy");
        assert!(remedy.contains("\"/e2e/**\""), "{remedy}");
    }

    #[test]
    fn an_already_anchored_pattern_is_not_double_anchored() {
        assert!(remedy_for("/e2e/**").contains("\"/e2e/**\""));
    }

    #[test]
    fn only_the_first_few_directories_are_named() {
        let mut rule = broad_rule();
        rule.matched = (0..6).map(|i| PathBuf::from(format!("d{i}/e2e"))).collect();
        rule.depths = 6;
        let summary = rule.finding().summary;
        assert!(summary.contains("d0/e2e") && summary.contains("d2/e2e"), "{summary}");
        assert!(!summary.contains("d3/e2e"), "the list is bounded: {summary}");
        assert!(
            summary.contains("and 3 more"),
            "the remainder is acknowledged: {summary}"
        );
    }

    #[test]
    fn a_config_without_excludes_is_not_walked_at_all() {
        let dir = tempfile::tempdir().unwrap();
        let report = scan(Some(dir.path()), &Config::default());
        assert!(report.root.is_none(), "nothing to scan, nothing scanned");
        assert!(report.broad_rules.is_empty());
    }

    #[test]
    fn no_findings_without_broad_rules() {
        let mut findings = Vec::new();
        diagnose(&ExcludeReport::skipped(), &mut findings);
        assert!(findings.is_empty());
    }
}
