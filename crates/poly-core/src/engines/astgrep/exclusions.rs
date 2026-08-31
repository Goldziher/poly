//! Per-rule default path exclusions, read out of the rule's **own YAML**.
//!
//! # The key that carries them is ast-grep's, not poly's
//!
//! `ast_grep_config::SerializableRuleConfig` (which `RuleConfig` derefs to)
//! already declares `files:` and `ignores:` — "glob patterns to specify that
//! the rule only applies to matching files" and "glob patterns that exclude
//! rules from applying to files" — and `from_yaml_string` deserializes both at
//! the pinned `0.45.2`. So a pack rule declares its exclusions in the standard
//! ast-grep schema, next to the `note:` that justifies them, and the pack's
//! single-parse-path invariant survives untouched: pack rules and user rules
//! still go through the identical `from_yaml_string` call, with no
//! poly-specific sidecar key and no second schema. A user's own rule file gets
//! the same treatment, which is what an ast-grep author already expects
//! `ignores:` to do.
//!
//! # What poly honors, and what it does not
//!
//! Only `ignores:` is wired in. `files:` — the *inclusion* filter — is not: it
//! inverts the question `[per-file-ignores]` answers ("which findings does this
//! path drop") into "which paths may this rule speak about at all", which the
//! post-lint filter cannot express without inventing a second mechanism. A
//! rule that needs to be scoped to a file set should say so in its `rule:`
//! matcher today.
//!
//! # These are defaults, and a user can take them over
//!
//! The globs collected here are handed to
//! [`crate::filter::PerFileIgnores::compile_with_defaults`] as *defaults*: a
//! `[per-file-ignores]` entry naming the rule replaces them outright. That is
//! the thing the pack's previous hardcoded exclusion table could not do — it
//! documented itself as a stand-in precisely because "a user cannot yet opt
//! back in for these ids on these paths".

use super::pack::builtin_pack;
use super::rules::{RuleMap, load_rules, rules_hash};
use crate::filter::DefaultPathIgnores;

/// Every `(rule_id, globs)` pair the active rule set declares via `ignores:`.
///
/// Built once per resolved config per run (not per file). Pack rules come
/// first and user rules overwrite them by `id`, mirroring
/// `super::merge_rules`: a user rule with a pack rule's id replaces it
/// outright, and a replacement that declares no `ignores:` therefore carries
/// none — the pack's globs must not survive a rule the user has taken over.
pub(crate) fn default_path_ignores(rules_dirs: &[String], builtin_pack_enabled: bool) -> DefaultPathIgnores {
    // Insertion-ordered by construction: pack ids first, then user ids, with a
    // user id overwriting the pack entry in place. A `Vec` beats a map here —
    // there are tens of rules, and the order is part of the contract.
    let mut collected: DefaultPathIgnores = Vec::new();
    if builtin_pack_enabled {
        collect(builtin_pack(), &mut collected);
    }
    if !rules_dirs.is_empty() {
        match load_rules(rules_dirs, &rules_hash(rules_dirs)) {
            Ok(user_rules) => collect(&user_rules, &mut collected),
            Err(error) => {
                tracing::debug!(%error, "cannot read user ast-grep rules for their declared path exclusions");
            }
        }
    }
    collected
}

/// Fold one rule map's declared `ignores:` into `out`, replacing any entry
/// already there for the same rule id.
fn collect(rules: &RuleMap, out: &mut DefaultPathIgnores) {
    for rule in rules.values().flatten() {
        let globs: Vec<globset::Glob> = read_globs(&rule.id, rule.ignores.as_deref());
        match out.iter_mut().find(|(id, _)| *id == rule.id) {
            // A rule that replaces another and declares no `ignores:` clears
            // the entry rather than inheriting it.
            Some(existing) => existing.1 = globs,
            None if globs.is_empty() => {}
            None => out.push((rule.id.clone(), globs)),
        }
    }
}

/// Read one rule's `ignores:` entries **through serde**, honoring ast-grep's
/// `{ glob, caseInsensitive }` object form as well as the bare string.
///
/// The entries are `ast_grep_config::rule_config::RuleFileGlob` values, and at
/// `0.45.2` that module is private and the type is not re-exported from the
/// crate root — so poly cannot name it, and therefore cannot match its
/// variants. It *is* `Serialize` and it is an untagged enum, so its own
/// serialization is the stable way to read it: a bare glob comes back as a
/// JSON string, the object form as an object. Round-tripping tens of rules
/// once per resolved config is not on any hot path.
///
/// A glob `globset` cannot parse is dropped with a warning rather than failing
/// the run: the rule it belongs to is still worth running, and a pack rule
/// with a bad glob is a poly defect caught by `poly rules test`, not something
/// a reader can act on mid-run.
fn read_globs<G: serde::Serialize>(rule_id: &str, entries: Option<&[G]>) -> Vec<globset::Glob> {
    let Some(entries) = entries else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| {
            let value = serde_json::to_value(entry)
                .inspect_err(|error| tracing::warn!(%rule_id, %error, "cannot read an `ignores:` entry"))
                .ok()?;
            let (glob, case_insensitive) = match &value {
                serde_json::Value::String(glob) => (glob.as_str(), false),
                serde_json::Value::Object(map) => (
                    map.get("glob").and_then(serde_json::Value::as_str)?,
                    map.get("caseInsensitive").and_then(serde_json::Value::as_bool).unwrap_or(false),
                ),
                _ => {
                    tracing::warn!(%rule_id, %value, "ignoring an `ignores:` entry that is neither a glob nor a glob config");
                    return None;
                }
            };
            globset::GlobBuilder::new(glob)
                .case_insensitive(case_insensitive)
                .build()
                .inspect_err(
                    |error| tracing::warn!(%rule_id, %glob, %error, "skipping unparseable `ignores:` glob in an ast-grep rule"),
                )
                .ok()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four rule ids the pack's former hardcoded `NOISY_PATH_EXCLUSIONS`
    /// table named, with the exact globs it gave each of them.
    ///
    /// This is the migration's safety net: the table moved into four YAML
    /// files, and the thing that would go unnoticed is a glob quietly changing
    /// shape on the way. Measured corpus behaviour depends on these exact
    /// strings — see each rule's own `note:`.
    const MIGRATED: &[(&str, &[&str])] = &[
        (
            "unwrap-used",
            &[
                "**/tests/**",
                "**/benches/**",
                "**/tests.rs",
                "**/test_support.rs",
                "**/*_generated.rs",
            ],
        ),
        ("placeholder-implementation", &["**/*_generated.rs"]),
        ("force-cast", &["**/RustBridge/**"]),
        (
            "not-null-assertion",
            &["**/test/**", "**/androidTest/**", "**/*Test.kt"],
        ),
    ];

    #[test]
    fn the_pack_declares_every_migrated_exclusion_in_yaml() {
        let declared = default_path_ignores(&[], true);
        for (rule_id, expected) in MIGRATED {
            let entry = declared
                .iter()
                .find(|(id, _)| id == rule_id)
                .unwrap_or_else(|| panic!("`{rule_id}` must declare `ignores:` in its own YAML"));
            let globs: Vec<&str> = entry.1.iter().map(globset::Glob::glob).collect();
            assert_eq!(&globs, expected, "`{rule_id}`'s declared globs changed");
        }
    }

    /// The globs are the ones the corpus measurements were taken against, so
    /// spot-check that they still match the paths those measurements name.
    #[test]
    fn the_migrated_globs_match_the_paths_they_were_measured_on() {
        let declared = default_path_ignores(&[], true);
        let matcher = |rule_id: &str, path: &str| {
            declared
                .iter()
                .find(|(id, _)| id == rule_id)
                .expect("rule declares ignores")
                .1
                .iter()
                .any(|glob| glob.compile_matcher().is_match(path))
        };
        assert!(matcher("placeholder-implementation", "crates/foo/src/frb_generated.rs"));
        assert!(!matcher("placeholder-implementation", "crates/foo/src/lib.rs"));
        assert!(matcher("unwrap-used", "crates/foo/tests/integration.rs"));
        assert!(matcher(
            "force-cast",
            "packages/swift/Sources/RustBridge/thing-swift.swift"
        ));
        assert!(matcher("not-null-assertion", "app/src/test/FooTest.kt"));
    }

    /// The pack is skippable: `[rules] builtin = false` takes its declared
    /// exclusions out with it, so a repo running only its own rules is not
    /// silently filtered by ids it never enabled.
    #[test]
    fn disabling_the_pack_withdraws_its_declared_exclusions() {
        assert!(default_path_ignores(&[], false).is_empty());
    }
}
