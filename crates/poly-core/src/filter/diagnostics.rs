//! Post-lint diagnostic filtering: `[per-file-ignores]` suppression and
//! per-rule severity remapping, both applied to the normalized `Diagnostic.code`
//! so they work uniformly across engines.
//!
//! # Two layers, and why one replaces the other
//!
//! [`PerFileIgnores`] holds two kinds of entry. A **user** entry comes from the
//! resolved `poly.toml`'s `[per-file-ignores]` table. A **default** entry comes
//! from a lint rule that declares its own path exclusions next to the rule it
//! belongs to — poly's built-in ast-grep pack ships several, as `ignores:` in
//! the rule YAML (see [`crate::engines::astgrep::exclusions`]).
//!
//! A user entry naming a rule **replaces** that rule's declared defaults rather
//! than unioning with them, matching the layering the pack already follows for
//! rules themselves: a user rule with a pack rule's `id` replaces it outright.
//! Union would leave a reader unable to *narrow* a default — writing the entry
//! they wanted would only ever add to a set they could not see — and that
//! inability to opt back in is exactly the limitation the pack's hardcoded
//! exclusion table was called out for.

use std::collections::BTreeMap;

use super::suppressed::{SuppressedDiagnostic, SuppressionReason};
use crate::engine::{Diagnostic, Severity};

/// Path exclusions a rule declares for itself: `(rule_id, globs)`, already
/// parsed so an unusable glob fails where it is written rather than per run.
pub(crate) type DefaultPathIgnores = Vec<(String, Vec<globset::Glob>)>;

/// One compiled ignore entry: a path glob, the rule codes it suppresses for a
/// matching file, and which layer it came from.
struct Entry {
    matcher: globset::GlobMatcher,
    rules: Vec<String>,
    reason: SuppressionReason,
}

/// Compiled `[per-file-ignores]`: each path glob paired with the rule codes to
/// suppress for files it matches. Built once per run, applied as a post-lint
/// filter on the normalized `Diagnostic.code` so it is engine-agnostic.
pub(crate) struct PerFileIgnores {
    entries: Vec<Entry>,
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
                    Ok(compiled) => Some(Entry {
                        matcher: compiled.compile_matcher(),
                        rules,
                        reason: SuppressionReason::PerFileIgnore,
                    }),
                    Err(error) => {
                        tracing::warn!(%glob, %error, "skipping invalid [per-file-ignores] glob");
                        None
                    }
                }
            })
            .collect();
        Self { entries }
    }

    /// [`compile`](Self::compile), plus the rule-declared `defaults` for every
    /// rule the user's own table does **not** speak about.
    ///
    /// "Speaks about" is the same exact-or-prefix test suppression itself uses
    /// ([`code_matches_rule`]), so a reader who writes `["placeholder"]` has
    /// taken over `placeholder-implementation` just as surely as one who spells
    /// the id out — the alternative is a config entry that appears to govern a
    /// rule while a default the reader cannot see keeps suppressing alongside it.
    pub(crate) fn compile_with_defaults(map: &BTreeMap<String, Vec<String>>, defaults: &DefaultPathIgnores) -> Self {
        let mut compiled = Self::compile(map);
        let user_rules: Vec<&str> = map
            .values()
            .flatten()
            .map(String::as_str)
            .filter(|rule| !rule.trim().is_empty())
            .collect();
        for (rule_id, globs) in defaults {
            if user_rules.iter().any(|rule| code_matches_rule(rule_id, rule)) {
                tracing::debug!(%rule_id, "[per-file-ignores] replaces this rule's declared default exclusions");
                continue;
            }
            compiled.entries.extend(globs.iter().map(|glob| Entry {
                matcher: glob.compile_matcher(),
                rules: vec![rule_id.clone()],
                reason: SuppressionReason::DefaultPathExclusion,
            }));
        }
        compiled
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Drop diagnostics whose `code` matches a rule listed for a glob the file
    /// matches, recording each drop in `suppressed`. `rel` is the file path
    /// relative to the run root, forward-slash normalized (per-file-ignore
    /// globs are repo-rooted). Each glob is evaluated once per file (not once
    /// per diagnostic).
    ///
    /// Matching is exact, or a ruff-style prefix where the boundary character is
    /// non-alphabetic — so `"F"` suppresses `F401` but not `FOO`, and
    /// `"too-many"` suppresses `too-many-methods`. This keeps a short prefix from
    /// silently swallowing an unrelated code from another engine.
    pub(crate) fn apply(
        &self,
        rel: &str,
        path: &std::path::Path,
        diagnostics: &mut Vec<Diagnostic>,
        suppressed: &mut Vec<SuppressedDiagnostic>,
    ) {
        // Nothing to filter is the common case now that rule-declared defaults
        // make this set non-empty on almost every run: a clean file must not
        // pay for the glob sweep below.
        if diagnostics.is_empty() {
            return;
        }
        let matched: Vec<&Entry> = self
            .entries
            .iter()
            .filter(|entry| entry.matcher.is_match(rel))
            .collect();
        if matched.is_empty() {
            return;
        }
        diagnostics.retain(|diagnostic| {
            let Some(code) = diagnostic.code.as_deref() else {
                return true;
            };
            let hit = matched
                .iter()
                .find(|entry| entry.rules.iter().any(|rule| code_matches_rule(code, rule)));
            match hit {
                Some(entry) => {
                    suppressed.push(SuppressedDiagnostic::new(path, diagnostic, entry.reason));
                    false
                }
                None => true,
            }
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
///
/// Shared with `filter::suppress` so an inline `poly: allow[…]` directive spells
/// its rule codes exactly the way `[per-file-ignores]` does.
pub(super) fn code_matches_rule(code: &str, rule: &str) -> bool {
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

    /// The file every unit test below reports against; the path only has to be
    /// carried through to the suppression record, not matched.
    fn path() -> &'static std::path::Path {
        std::path::Path::new("src/foo.py")
    }

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
        let mut suppressed = Vec::new();
        ignores.apply("tests/unit/foo.py", path(), &mut diags, &mut suppressed);
        let codes: Vec<_> = diags.iter().map(|d| d.code.clone()).collect();
        assert_eq!(codes, vec![Some("E501".to_string()), None]);
        assert_eq!(
            suppressed
                .iter()
                .map(|s| (s.code.clone(), s.reason))
                .collect::<Vec<_>>(),
            vec![
                (Some("F401".to_string()), SuppressionReason::PerFileIgnore),
                (Some("too-many-methods".to_string()), SuppressionReason::PerFileIgnore),
            ],
            "every dropped diagnostic is recorded, and names the mechanism"
        );

        let mut diags = vec![diag(Some("F401"))];
        let mut suppressed = Vec::new();
        ignores.apply("src/foo.py", path(), &mut diags, &mut suppressed);
        assert_eq!(diags.len(), 1, "non-matching path is untouched");
        assert!(suppressed.is_empty(), "and nothing is recorded as suppressed");
    }

    /// A rule's own declared exclusions apply with no user config at all, and
    /// say which layer dropped the finding.
    #[test]
    fn a_rule_declared_default_suppresses_and_names_itself() {
        let defaults = vec![(
            "placeholder-implementation".to_string(),
            vec![globset::Glob::new("**/*_generated.rs").unwrap()],
        )];
        let ignores = PerFileIgnores::compile_with_defaults(&BTreeMap::new(), &defaults);

        let mut diags = vec![diag(Some("placeholder-implementation"))];
        let mut suppressed = Vec::new();
        ignores.apply("crates/foo/src/frb_generated.rs", path(), &mut diags, &mut suppressed);
        assert!(diags.is_empty(), "the declared glob suppresses");
        assert_eq!(suppressed.len(), 1);
        assert_eq!(suppressed[0].reason, SuppressionReason::DefaultPathExclusion);

        let mut diags = vec![diag(Some("placeholder-implementation"))];
        let mut suppressed = Vec::new();
        ignores.apply("crates/foo/src/lib.rs", path(), &mut diags, &mut suppressed);
        assert_eq!(diags.len(), 1, "ordinary source is untouched");
        assert!(suppressed.is_empty());
    }

    /// Precedence is replace, not union: a user entry naming the rule takes the
    /// declared defaults out of force entirely.
    #[test]
    fn a_user_entry_replaces_a_rules_declared_defaults() {
        let defaults = vec![(
            "placeholder-implementation".to_string(),
            vec![globset::Glob::new("**/*_generated.rs").unwrap()],
        )];
        let mut map = BTreeMap::new();
        map.insert("vendor/**".to_string(), vec!["placeholder-implementation".to_string()]);
        let ignores = PerFileIgnores::compile_with_defaults(&map, &defaults);

        let mut diags = vec![diag(Some("placeholder-implementation"))];
        let mut suppressed = Vec::new();
        ignores.apply("crates/foo/src/frb_generated.rs", path(), &mut diags, &mut suppressed);
        assert_eq!(diags.len(), 1, "the replaced default no longer suppresses");
        assert!(suppressed.is_empty());

        let mut diags = vec![diag(Some("placeholder-implementation"))];
        let mut suppressed = Vec::new();
        ignores.apply("vendor/thing.rs", path(), &mut diags, &mut suppressed);
        assert!(diags.is_empty(), "the user's own glob is what suppresses now");
        assert_eq!(suppressed[0].reason, SuppressionReason::PerFileIgnore);
    }

    /// A user entry that only *prefix*-matches the rule id still takes it over:
    /// a rule the reader has spoken about is a rule they own.
    #[test]
    fn a_prefix_user_entry_also_replaces_the_default() {
        let defaults = vec![(
            "placeholder-implementation".to_string(),
            vec![globset::Glob::new("**/*_generated.rs").unwrap()],
        )];
        let mut map = BTreeMap::new();
        map.insert("vendor/**".to_string(), vec!["placeholder".to_string()]);
        let ignores = PerFileIgnores::compile_with_defaults(&map, &defaults);

        let mut diags = vec![diag(Some("placeholder-implementation"))];
        let mut suppressed = Vec::new();
        ignores.apply("crates/foo/src/frb_generated.rs", path(), &mut diags, &mut suppressed);
        assert_eq!(diags.len(), 1, "the prefix entry replaced the default");
        assert!(suppressed.is_empty());
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
        let mut suppressed = Vec::new();
        ignores.apply("anything.py", path(), &mut diags, &mut suppressed);
        assert_eq!(diags.len(), 2, "nothing is suppressed");
        assert!(suppressed.is_empty());
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
