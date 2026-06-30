//! Discovery- and result-filtering helpers used by the runner: exclude-glob
//! merging, `[per-file-ignores]` suppression, and generated-lock-file
//! detection. Kept out of `runner.rs` so orchestration stays one concern per
//! file (and under the module line cap).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::engine::Diagnostic;

/// The full exclude set for a run: `[discovery] exclude` from config, plus any
/// call-time `--exclude` / MCP globs. Built once per run (not in the hot loop).
pub(crate) fn merged_excludes(config_exclude: &[String], extra: &[String]) -> Vec<String> {
    if extra.is_empty() {
        return config_exclude.to_vec();
    }
    let mut excludes = config_exclude.to_vec();
    excludes.extend(extra.iter().cloned());
    excludes
}

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
                let rules: Vec<String> = rules
                    .iter()
                    .filter(|rule| !rule.trim().is_empty())
                    .cloned()
                    .collect();
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
        // Evaluate each glob once for this file; collect the rule lists that hit.
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

/// File path relative to the run root, forward-slash normalized, for matching
/// repo-rooted `[per-file-ignores]` globs. Strips the first of `bases` (cwd plus
/// the explicitly passed roots) that prefixes the path, so both `poly lint .`
/// (relative paths) and `poly lint /abs/repo` (absolute paths) resolve to a
/// repo-relative path the globs can match.
pub(crate) fn relative_for_match(path: &Path, bases: &[PathBuf]) -> String {
    let mut rel = path;
    for base in bases {
        if let Ok(stripped) = path.strip_prefix(base) {
            rel = stripped;
            break;
        }
    }
    let rel = rel.strip_prefix(".").unwrap_or(rel);
    let text = rel.to_string_lossy();
    // Avoid the allocation+rescan of `replace` on the common (no-backslash) path.
    if text.contains('\\') {
        text.replace('\\', "/")
    } else {
        text.into_owned()
    }
}

/// Prefix bases for [`relative_for_match`]: the working directory (when
/// available) followed by the explicitly passed roots, so per-file-ignore globs
/// resolve against whichever one prefixes a discovered file.
pub(crate) fn match_bases(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut bases = Vec::with_capacity(paths.len() + 1);
    match std::env::current_dir() {
        Ok(cwd) => bases.push(cwd),
        Err(error) => {
            tracing::warn!(%error, "cannot determine working directory; \
                 per-file-ignores fall back to matching against the passed paths");
        }
    }
    bases.extend(paths.iter().cloned());
    bases
}

/// Generated lock files, by exact name, that `poly fmt` never rewrites on a
/// directory walk. Any `*.lock` file is also treated as a lock file; these are
/// the ones whose names do not end in `.lock`.
const LOCKFILE_NAMES: &[&str] = &[
    "package-lock.json",
    "npm-shrinkwrap.json",
    "pnpm-lock.yaml",
    "bun.lockb",
];

/// Whether `path` is a machine-generated lock file that must not be reformatted.
/// Matched by the `*.lock` extension (Cargo.lock, yarn.lock, poetry.lock,
/// uv.lock, composer.lock, Gemfile.lock, flake.lock, deno.lock, …) or by an
/// exact name in [`LOCKFILE_NAMES`] for the lock files that don't end in `.lock`.
pub(crate) fn is_generated_lockfile(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    name.ends_with(".lock") || LOCKFILE_NAMES.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Severity;

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
    fn merged_excludes_unions_config_and_opts() {
        assert_eq!(
            merged_excludes(&["test_apps/**".to_string()], &[]),
            vec!["test_apps/**"]
        );
        assert_eq!(
            merged_excludes(&["test_apps/**".to_string()], &["artifacts/**".to_string()]),
            vec!["test_apps/**".to_string(), "artifacts/**".to_string()],
        );
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
        assert!(
            !code_matches_rule("ERR_X", "E"),
            "alphabetic boundary blocks"
        );
        assert!(!code_matches_rule("FOO", "F"), "alphabetic boundary blocks");
    }

    #[test]
    fn empty_rule_string_is_dropped_not_a_wildcard() {
        let mut map = BTreeMap::new();
        map.insert("**".to_string(), vec![String::new(), "  ".to_string()]);
        let ignores = PerFileIgnores::compile(&map);
        assert!(
            ignores.is_empty(),
            "an entry with only blank codes is skipped entirely"
        );
        let mut diags = vec![diag(Some("F401")), diag(None)];
        ignores.apply("anything.py", &mut diags);
        assert_eq!(diags.len(), 2, "nothing is suppressed");
    }

    #[test]
    fn relative_for_match_strips_cwd_and_passed_roots() {
        let cwd = PathBuf::from("/work/repo");
        assert_eq!(
            relative_for_match(
                Path::new("/work/repo/tests/a.py"),
                std::slice::from_ref(&cwd)
            ),
            "tests/a.py"
        );
        let bases = vec![cwd, PathBuf::from("/other/root")];
        assert_eq!(
            relative_for_match(Path::new("/other/root/tests/a.py"), &bases),
            "tests/a.py"
        );
        assert_eq!(
            relative_for_match(Path::new("./tests/a.py"), &[PathBuf::from("/x")]),
            "tests/a.py"
        );
    }

    #[test]
    fn recognizes_generated_lock_files() {
        for name in [
            "Cargo.lock",
            "yarn.lock",
            "poetry.lock",
            "uv.lock",
            "Gemfile.lock",
            "flake.lock",
            "composer.lock",
            "package-lock.json",
            "pnpm-lock.yaml",
            "npm-shrinkwrap.json",
            "bun.lockb",
        ] {
            assert!(
                is_generated_lockfile(Path::new(name)),
                "{name} should be treated as a lock file"
            );
        }
        for name in ["main.rs", "Cargo.toml", "package.json", "lockfile.txt"] {
            assert!(
                !is_generated_lockfile(Path::new(name)),
                "{name} must not be treated as a lock file"
            );
        }
    }
}
