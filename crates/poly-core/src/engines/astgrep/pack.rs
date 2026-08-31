//! poly's built-in ast-grep rule pack: curated, per-language lint rules
//! embedded in the binary so `poly lint` has real coverage with zero setup —
//! unlike user rules, which require `[rules] dirs` to point at YAML files on
//! disk.
//!
//! # One parse path, not two
//!
//! Every pack rule is compiled through the exact same
//! `ast_grep_config::from_yaml_string` call [`super::rules::load_flat`] uses
//! for user rules — there is no separate pack-specific YAML schema or parser.
//! The only difference is *where* the YAML text comes from: `include_str!`
//! (a compile-time constant) instead of `std::fs::read_to_string` (re-read
//! per content hash). That is also why the pack is cached forever in a
//! `OnceLock` rather than keyed by content hash like [`super::rules::load_rules`]
//! — it cannot change without a new poly binary, so there is nothing to key on.
//!
//! # Layering: pack beneath user rules
//!
//! [`super`]'s `resolve_rules` merges the pack with a language's
//! user rules by `id`: a user rule with the same `id` as a pack rule replaces
//! it outright (the pack fills gaps in a repo's own rules, it does not
//! override them). Whether a pack rule itself is "on" or "off" by default is
//! authored directly in its YAML `severity:` field (see `builtin/*/*.yml`);
//! `[lint.astgrep] select` / `extend_select` / `ignore` can still turn any of
//! them on or off, same as a user rule.
//!
//! # The path-exclusion problem
//!
//! An ast-grep rule matches AST nodes, not file paths, so its `rule:` matcher
//! cannot itself express "skip this finding in `tests/`". Measured against the
//! xberg-io corpus (48 roots — see the crate's `poly rules test` corpus notes):
//! `placeholder-implementation` had 320 of its 324 (99%) findings in
//! `frb_generated.rs` (flutter_rust_bridge's generated FFI glue). `force-cast`
//! had 662 of 662 (100%) inside `packages/swift/Sources/RustBridge/*-swift.swift`
//! — files stamped `// swift-format-ignore-file` and named by the
//! `swift-bridge` code generator's own convention. `not-null-assertion` had
//! 103 of 103 (100%) under Kotlin test source sets. (`unwrap-used`,
//! `allow-attribute-without-reason`, and `undocumented-unsafe-block` also had
//! large path-shaped noise components; `unwrap-used` carries its exclusions,
//! the other two shipped `off` after the default-on audit below — see their
//! own YAML notes.)
//!
//! Each such rule declares those paths **in its own YAML**, as the standard
//! ast-grep `ignores:` key, next to the `note:` that justifies them. See
//! [`super::exclusions`] for why that key rather than a poly-specific one, and
//! for how the globs reach the runner. They are *defaults*: a
//! `[per-file-ignores]` entry naming the rule replaces them, so a reader can
//! opt back in — which the hardcoded Rust table this replaced could not do,
//! and said so.
//!
//! The rule this pass held to still applies: a rule may lean on a path
//! exclusion only while a *minority* of its corpus exposure is unaddressable
//! noise; a rule where path exclusion alone cannot get it under control ships
//! `off` instead (`undocumented-unsafe-block` is the example — see below).
//!
//! # Default-on audit (per-rule corpus exposure and FP rate)
//!
//! Every rule shipping a default severity other than `off` was audited
//! against the corpus above; a rule with very low or zero exposure is
//! reported as such rather than inferred clean from silence:
//!
//! | rule | corpus (raw → after exclusion) | hand-read FP rate |
//! |---|---|---|
//! | `swallowed-error` | 12 → 3 (9 in test paths, not excluded — kept as low-volume signal) | 0/3 non-test + 2/2 test-path spot-checked, all genuine silent-discard patterns |
//! | `blocking-call-in-async-fn` | 9 → 9 (no test-path hits) | 0/9 FP on inspection; one apparent exemption failure (`block_in_place`-wrapped call) turned out to be a stale line number against a live-edited repo — a fresh re-run found the real match was a second, unwrapped `block_on` call in an `else` branch a screen below, genuinely unguarded |
//! | `force-cast` (swift) | 662 → 0 (100% in `RustBridge/*-swift.swift` generated glue) | no corpus exposure on hand-written Swift |
//! | `force-try` (swift) | 1 → 0 (its one finding is in an `alef`-generated file) | no corpus exposure on hand-written Swift |
//! | `not-null-assertion` (kotlin) | 103 → 0 (100% under `**/test/**`/`*Test.kt`) | no corpus exposure on non-test Kotlin |
//! | `rescue-modifier` (ruby) | 11 | already hand-read 0/11 FP by this rule's own author — see `builtin/ruby/rescue-modifier.yml`'s note |
//! | `eval-usage` (ruby), `preserve-stack-trace`/`empty-catch-block` (java), `async-void`/`rethrow-loses-stack` (csharp), `unused-operation` (elixir) | 0 or 1 (the one `empty-catch-block` hit is in a vendored Maven wrapper script, not repo-authored code) | no corpus exposure; kept on as narrow, established language-specific anti-patterns pending real-world validation |
//! | `defer-in-loop` (go) | 1 | too little volume to characterize; kept on |
//!
//! `allow-attribute-without-reason`, `unwrap-used`, and
//! `undocumented-unsafe-block` shipped `off` after this audit; see their own
//! YAML `note:` fields for the full reasoning. Briefly: `allow-attribute-
//! without-reason` was the single largest finding count of all 26 rules —
//! 13,622 raw — dominated by self-explanatory FFI/generated-code allows.
//! `undocumented-unsafe-block`'s 5,475-after-exclusion residual classified as
//! 94.4% vendored FFI-binding code (including a crate labeled a "maintained
//! fork" of an upstream dependency), 4.5% `unsafe { env::set_var(..) }` inside
//! `#[cfg(test)]` (a carve-out gap, not a path-shaped one), and only ~1%
//! genuinely first-party production code — every finding was individually
//! correct (100% of a sample lacked a real `SAFETY` comment), but correctness
//! is not the same question as whether the reader can act on it.

use std::collections::HashMap;
use std::sync::OnceLock;

use ast_grep_config::{GlobalRules, RuleConfig, from_yaml_string};

use super::language::TslpLanguage;
use super::rules::RuleMap;

/// Every built-in rule's YAML source, embedded at compile time. Order does
/// not matter — [`builtin_pack`] groups by `language:` regardless.
const PACK_RULES: &[&str] = &[
    include_str!("builtin/csharp/async-void.yml"),
    include_str!("builtin/csharp/rethrow-loses-stack.yml"),
    include_str!("builtin/csharp/sync-over-async.yml"),
    include_str!("builtin/elixir/unused-operation.yml"),
    include_str!("builtin/go/deep-exit.yml"),
    include_str!("builtin/go/defer-in-loop.yml"),
    include_str!("builtin/go/force-type-assert.yml"),
    include_str!("builtin/java/empty-catch-block.yml"),
    include_str!("builtin/java/implicit-switch-fallthrough.yml"),
    include_str!("builtin/java/preserve-stack-trace.yml"),
    include_str!("builtin/kotlin/not-null-assertion.yml"),
    include_str!("builtin/python/string-concat-in-loop.yml"),
    include_str!("builtin/python/todo-marker.yml"),
    include_str!("builtin/ruby/eval-usage.yml"),
    include_str!("builtin/ruby/rescue-modifier.yml"),
    include_str!("builtin/rust/allow-attribute-without-reason.yml"),
    include_str!("builtin/rust/blocking-call-in-async-fn.yml"),
    include_str!("builtin/rust/expect-used.yml"),
    include_str!("builtin/rust/placeholder-implementation.yml"),
    include_str!("builtin/rust/swallowed-error.yml"),
    include_str!("builtin/rust/test-without-assertion.yml"),
    include_str!("builtin/rust/todo-marker.yml"),
    include_str!("builtin/rust/undocumented-unsafe-block.yml"),
    include_str!("builtin/rust/unwrap-used.yml"),
    include_str!("builtin/swift/force-cast.yml"),
    include_str!("builtin/swift/force-try.yml"),
];

static BUILTIN_PACK: OnceLock<RuleMap> = OnceLock::new();

/// The embedded pack, parsed once per process and grouped by language name —
/// the same shape [`super::rules::load_rules`] returns for user rules, so
/// callers merge the two uniformly.
///
/// Each pack YAML file is parsed with its own fresh [`GlobalRules`], matching
/// [`super::rules::load_flat`]'s "no cross-file `refers:`" limitation for user
/// rules: every pack rule is self-contained.
pub(super) fn builtin_pack() -> &'static RuleMap {
    BUILTIN_PACK.get_or_init(|| {
        let globals = GlobalRules::default();
        let mut map: RuleMap = HashMap::new();
        for yaml in PACK_RULES {
            let rules: Vec<RuleConfig<TslpLanguage>> = from_yaml_string(yaml, &globals).unwrap_or_else(|error| {
                panic!(
                    "poly's built-in ast-grep pack failed to parse ({error}); this is a poly \
                     release defect, not a user-fixable error — the pack is compiled into the \
                     binary and validated by `poly rules test` in CI"
                )
            });
            for rule in rules {
                map.entry(rule.language.name().to_string()).or_default().push(rule);
            }
        }
        map
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_pack_covers_every_shipped_language() {
        let pack = builtin_pack();
        for lang in [
            "csharp", "elixir", "go", "java", "kotlin", "python", "ruby", "rust", "swift",
        ] {
            assert!(
                pack.get(lang).is_some_and(|rules| !rules.is_empty()),
                "expected at least one built-in rule for {lang}"
            );
        }
    }

    #[test]
    fn builtin_pack_has_26_rules() {
        let total: usize = builtin_pack().values().map(Vec::len).sum();
        assert_eq!(total, 26, "expected 26 built-in rules across 9 languages");
    }

    /// The Rust rules that carve test code out of their matches each define the
    /// same four helpers, and they must stay identical.
    ///
    /// Extraction into ast-grep *global* utils is the real fix (issue #22), and
    /// it is blocked on a deserializer poly-core does not have: a relational-only
    /// predicate is legal as a `utils:` entry and illegal as a top-level `rule:`,
    /// so registering one needs `parse_global_utils`, which needs
    /// `SerializableGlobalRule` deserialized from YAML. `serde_yaml` — what
    /// ast-grep itself uses — is unmaintained, and swapping in a different YAML
    /// deserializer for types this dependent on `#[serde(flatten)]` and untagged
    /// enums is not a change to make casually inside the pack's single-parse-path
    /// invariant.
    ///
    /// So the copies stay, and this makes the thing that actually hurts —
    /// *silent* divergence between them — a test failure instead. Four copies is
    /// four chances to drift, and drift here is invisible until someone
    /// hand-reads several thousand findings.
    #[test]
    fn the_shared_rust_test_context_helpers_have_not_drifted() {
        const SHARED_HELPERS: &[&str] = &[
            "  test-attribute:",
            "  cfg-test-attribute:",
            "  attribute-or-comment:",
            "  in-test-context:",
        ];
        const CARRIERS: &[(&str, &str)] = &[
            ("unwrap-used", include_str!("builtin/rust/unwrap-used.yml")),
            ("expect-used", include_str!("builtin/rust/expect-used.yml")),
            (
                "placeholder-implementation",
                include_str!("builtin/rust/placeholder-implementation.yml"),
            ),
            (
                "blocking-call-in-async-fn",
                include_str!("builtin/rust/blocking-call-in-async-fn.yml"),
            ),
            (
                "undocumented-unsafe-block",
                include_str!("builtin/rust/undocumented-unsafe-block.yml"),
            ),
        ];

        let (reference_id, reference_yaml) = CARRIERS[0];
        let reference = extract_helpers(reference_yaml);
        assert_eq!(
            reference.len(),
            SHARED_HELPERS.len(),
            "{reference_id} must define every shared helper"
        );
        for (id, yaml) in &CARRIERS[1..] {
            assert_eq!(
                extract_helpers(yaml),
                reference,
                "`{id}` defines the shared test-context helpers differently from `{reference_id}`; \
                 they must stay identical while the definition is duplicated"
            );
        }
    }

    /// Pull each shared helper's block out of a rule's `utils:` section, keyed by
    /// name so a rule that also defines helpers of its own (as
    /// `blocking-call-in-async-fn` does) still compares equal on the shared ones.
    fn extract_helpers(yaml: &str) -> std::collections::BTreeMap<String, Vec<String>> {
        const SHARED: &[&str] = &[
            "test-attribute",
            "cfg-test-attribute",
            "attribute-or-comment",
            "in-test-context",
        ];
        let mut helpers = std::collections::BTreeMap::new();
        let mut current: Option<String> = None;
        let mut body: Vec<String> = Vec::new();
        let mut in_utils = false;
        for line in yaml.lines() {
            if line == "utils:" {
                in_utils = true;
                continue;
            }
            if !in_utils {
                continue;
            }
            // A column-0 key ends the `utils:` block.
            let ends_block = !line.is_empty() && !line.starts_with(' ');
            let starts_helper = line.starts_with("  ") && !line.starts_with("   ") && line.trim_end().ends_with(':');
            if ends_block || starts_helper {
                if let Some(name) = current.take()
                    && SHARED.contains(&name.as_str())
                {
                    helpers.insert(name, std::mem::take(&mut body));
                } else {
                    body.clear();
                }
                if ends_block {
                    break;
                }
                current = Some(line.trim().trim_end_matches(':').to_string());
                continue;
            }
            if current.is_some() {
                body.push(line.to_string());
            }
        }
        if let Some(name) = current
            && SHARED.contains(&name.as_str())
        {
            helpers.insert(name, body);
        }
        helpers
    }
}
