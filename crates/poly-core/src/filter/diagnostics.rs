//! Post-lint diagnostic filtering: `[per-file-ignores]` suppression and
//! per-rule severity remapping, both applied to the normalized `Diagnostic.code`
//! so they work uniformly across engines.

use std::collections::BTreeMap;

use crate::engine::{Diagnostic, Severity};

/// Compiled `[per-file-ignores]`: each path glob paired with the rule codes to
/// suppress for files it matches. Built once per run, applied as a post-lint
/// filter on the normalized `Diagnostic.code` so it is engine-agnostic.
pub(crate) struct PerFileIgnores {
    entries: Vec<(globset::GlobMatcher, Vec<String>)>,
}

impl PerFileIgnores {
    /// Compile the config map; an invalid glob — or an entry whose rule list is
    /// empty after dropping blank codes — is skipped with a warning rather than
    /// failing the run. Dropping blank codes is a safety guard: an empty rule
    /// string would make the prefix test below match every code and silently
    /// suppress all diagnostics for the glob.
    pub(crate) fn compile(map: &BTreeMap<String, Vec<String>>) -> Self {
        let entries = map
            .iter()
            .filter_map(|(glob, rules)| {
                let rules: Vec<String> = rules.iter().filter(|rule| !rule.trim().is_empty()).cloned().collect();
                if rules.is_empty() {
                    tracing::warn!(%glob, "skipping [per-file-ignores] entry: no non-empty rule codes");
                    return None;
                }
                match globset::Glob::new(glob) {
                    Ok(compiled) => Some((compiled.compile_matcher(), rules)),
                    Err(error) => {
                        tracing::warn!(%glob, %error, "skipping invalid [per-file-ignores] glob");
                        None
                    }
                }
            })
            .collect();
        Self { entries }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Drop diagnostics whose `code` matches a rule listed for a glob the file
    /// matches. `rel` is the file path relative to the run root, forward-slash
    /// normalized (per-file-ignore globs are repo-rooted). Each glob is evaluated
    /// once per file (not once per diagnostic).
    ///
    /// Matching is exact, or a ruff-style prefix where the boundary character is
    /// non-alphabetic — so `"F"` suppresses `F401` but not `FOO`, and
    /// `"too-many"` suppresses `too-many-methods`. This keeps a short prefix from
    /// silently swallowing an unrelated code from another engine.
    pub(crate) fn apply(&self, rel: &str, diagnostics: &mut Vec<Diagnostic>) {
        let matched: Vec<&[String]> = self
            .entries
            .iter()
            .filter(|(matcher, _)| matcher.is_match(rel))
            .map(|(_, rules)| rules.as_slice())
            .collect();
        if matched.is_empty() {
            return;
        }
        diagnostics.retain(|diagnostic| {
            let Some(code) = diagnostic.code.as_deref() else {
                return true;
            };
            !matched
                .iter()
                .any(|rules| rules.iter().any(|rule| code_matches_rule(code, rule)))
        });
    }
}

/// Per-rule severity remap built from the `[lint.<lang>.<tool>.rules.<code>]
/// level` entries. Applied as a post-lint pass on the normalized
/// `Diagnostic.code` so it works uniformly for every engine — including those
/// with no native severity config. Mirrors [`PerFileIgnores`]: compile once per
/// engine plan, then apply per file.
pub(crate) struct SeverityRemap {
    /// `(rule_code, level)` pairs in config order; the first whose code matches a
    /// diagnostic wins. Blank rule codes are dropped on construction so a stray
    /// empty string cannot prefix-match (and thus remap) every code.
    entries: Vec<(String, Severity)>,
}

impl SeverityRemap {
    /// Build from the per-rule `level` pairs. Entries with a blank rule code are
    /// dropped: an empty rule would prefix-match every code and silently remap
    /// all diagnostics.
    pub(crate) fn new(entries: Vec<(String, Severity)>) -> Self {
        let entries = entries
            .into_iter()
            .filter(|(rule, _)| !rule.trim().is_empty())
            .collect();
        Self { entries }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Set each diagnostic's severity to the level of the FIRST rule whose code
    /// matches its `code` (via [`code_matches_rule`], the same exact-or-prefix
    /// semantics as per-file-ignores). Diagnostics without a code, or with no
    /// matching rule, are left untouched.
    pub(crate) fn apply(&self, diagnostics: &mut [Diagnostic]) {
        if self.entries.is_empty() {
            return;
        }
        for diagnostic in diagnostics.iter_mut() {
            let Some(code) = diagnostic.code.as_deref() else {
                continue;
            };
            let level = self
                .entries
                .iter()
                .find(|(rule, _)| code_matches_rule(code, rule))
                .map(|(_, level)| *level);
            if let Some(level) = level {
                diagnostic.severity = level;
            }
        }
    }
}

/// Whether `code` is suppressed by a per-file-ignore `rule`: exact match, or a
/// prefix match where the next character is not alphabetic (ruff-style code
/// families like `F` → `F401`, while `E` does not swallow `ERR_X`).
fn code_matches_rule(code: &str, rule: &str) -> bool {
    if code == rule {
        return true;
    }
    match code.strip_prefix(rule) {
        Some(rest) => rest.chars().next().is_none_or(|c| !c.is_alphabetic()),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diag(code: Option<&str>) -> Diagnostic {
        Diagnostic {
            engine: "test".to_string(),
            code: code.map(str::to_owned),
            severity: Severity::Warning,
            title: "x".to_string(),
            description: None,
            span: None,
            url: None,
            fix: Vec::new(),
            metadata: BTreeMap::new(),
        }
    }

    #[test]
    fn per_file_ignores_suppress_matching_codes() {
        let mut map = BTreeMap::new();
        map.insert(
            "tests/**".to_string(),
            vec!["F401".to_string(), "too-many-methods".to_string()],
        );
        let ignores = PerFileIgnores::compile(&map);

        let mut diags = vec![
            diag(Some("F401")),
            diag(Some("too-many-methods")),
            diag(Some("E501")),
            diag(None),
        ];
        ignores.apply("tests/unit/foo.py", &mut diags);
        let codes: Vec<_> = diags.iter().map(|d| d.code.clone()).collect();
        assert_eq!(codes, vec![Some("E501".to_string()), None]);

        let mut diags = vec![diag(Some("F401"))];
        ignores.apply("src/foo.py", &mut diags);
        assert_eq!(diags.len(), 1, "non-matching path is untouched");
    }

    #[test]
    fn prefix_match_respects_a_non_alphabetic_boundary() {
        assert!(code_matches_rule("E501", "E"), "E501 is in the E family");
        assert!(code_matches_rule("too-many-methods", "too-many"));
        assert!(code_matches_rule("F401", "F401"), "exact match");
        assert!(!code_matches_rule("ERR_X", "E"), "alphabetic boundary blocks");
        assert!(!code_matches_rule("FOO", "F"), "alphabetic boundary blocks");
    }

    #[test]
    fn empty_rule_string_is_dropped_not_a_wildcard() {
        let mut map = BTreeMap::new();
        map.insert("**".to_string(), vec![String::new(), "  ".to_string()]);
        let ignores = PerFileIgnores::compile(&map);
        assert!(ignores.is_empty(), "an entry with only blank codes is skipped entirely");
        let mut diags = vec![diag(Some("F401")), diag(None)];
        ignores.apply("anything.py", &mut diags);
        assert_eq!(diags.len(), 2, "nothing is suppressed");
    }

    #[test]
    fn severity_remap_sets_first_matching_rule_level() {
        let remap = SeverityRemap::new(vec![("F401".to_string(), Severity::Warning)]);
        assert!(!remap.is_empty());

        let mut diags = vec![diag(Some("F401")), diag(Some("E501")), diag(None)];
        diags[0].severity = Severity::Error;
        remap.apply(&mut diags);

        assert_eq!(diags[0].severity, Severity::Warning, "F401 is remapped");
        assert_eq!(
            diags[1].severity,
            Severity::Warning,
            "a non-matching code keeps its severity"
        );
        assert_eq!(diags[2].severity, Severity::Warning, "a code-less diag is untouched");
    }

    #[test]
    fn severity_remap_honors_prefix_family_and_first_match_wins() {
        let remap = SeverityRemap::new(vec![
            ("F".to_string(), Severity::Hint),
            ("F401".to_string(), Severity::Error),
        ]);
        let mut diags = vec![diag(Some("F401"))];
        diags[0].severity = Severity::Warning;
        remap.apply(&mut diags);
        assert_eq!(
            diags[0].severity,
            Severity::Hint,
            "the first matching rule wins (family prefix before the exact code)"
        );
        let mut other = vec![diag(Some("FOO"))];
        other[0].severity = Severity::Warning;
        remap.apply(&mut other);
        assert_eq!(other[0].severity, Severity::Warning, "FOO is not in the F family");
    }

    #[test]
    fn severity_remap_empty_is_a_noop() {
        let remap = SeverityRemap::new(Vec::new());
        assert!(remap.is_empty());
        let mut diags = vec![diag(Some("F401"))];
        diags[0].severity = Severity::Error;
        remap.apply(&mut diags);
        assert_eq!(diags[0].severity, Severity::Error, "empty remap changes nothing");
    }
}
