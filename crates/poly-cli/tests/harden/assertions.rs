//! What fails a harden run.
//!
//! Every check appends to one vector and the run reports them together. A
//! harness that stops at the first failure tells you one of the five things that
//! broke, and you find the rest one run at a time.
//!
//! The split that governs this file: an **invariant** holds regardless of what a
//! tree contains, so it can be asserted on any corpus. A **count** is a property
//! of the tree, so it can only gate against a pinned input. Gating a count on a
//! moving corpus produces a check that fires on someone else's commit and gets
//! disabled within a month, taking the invariants with it.

use super::record::RootRecord;

/// Languages poly claims to lint. A `no lint rules for <language>` skip for any
/// of these is a regression in `provides_language_lint`, not a fact about the
/// tree — which is why it is an invariant and gates on every corpus.
///
/// Hand-maintained on purpose. Deriving it from `Language` or from a previous
/// run would make it shrink silently exactly when detection regresses, which is
/// the one direction that must never pass unnoticed.
const LINT_CLAIMED_LANGUAGES: &[&str] = &[
    "Python",
    "JavaScript",
    "TypeScript",
    "JSX",
    "TSX",
    "JSON",
    "YAML",
    "TOML",
    "Markdown",
    "SQL",
    "CSS",
    "SCSS",
    "GraphQL",
    "PHP",
    "Rust",
    "Go",
    "Java",
    "Kotlin",
    "C",
    "C++",
    "C#",
    "Ruby",
];

/// Collect every failed assertion for this root.
pub fn check(record: &RootRecord) -> Vec<String> {
    let mut failures = Vec::new();

    // A file poly failed on was not checked, whatever else the run reports.
    if record.lint.errored > 0 {
        failures.push(format!("lint failed on {} file(s)", record.lint.errored));
    }
    if record.fmt.errored > 0 {
        failures.push(format!("format failed on {} file(s)", record.fmt.errored));
    }

    // "Discovered nothing" is the failure that looks most like success.
    if record.lint.files_discovered == 0 {
        failures.push("lint discovered zero files; the measurement is empty, not clean".to_string());
    }

    // Formatting must reach a fixed point, or `fmt --fix` then `fmt --check` is
    // never clean and two backends are fighting over the same file.
    if record.idempotence.pass2_changed > 0 {
        failures.push(format!(
            "formatting is not idempotent: {} file(s) changed on a second pass ({})",
            record.idempotence.pass2_changed,
            sample(&record.idempotence.non_idempotent)
        ));
    }
    if record.idempotence.errored > 0 {
        failures.push(format!(
            "the second format pass errored on {} file(s); a formatter emitted output its own parser rejects",
            record.idempotence.errored
        ));
    }

    // A tree that changed under the measurement invalidates every comparison it
    // made, so the cache checks are not asserted — reporting a disagreement
    // there would be blaming poly for somebody saving a file.
    let tree_moved = record.dirty != record.dirty_after;
    if tree_moved {
        return failures;
    }

    // A warm run that disagrees with a cold one means the cache is not a cache.
    if !record.cache.warm_output_identical {
        failures.push("a warm run disagreed with the cold run: the cache key is unstable".to_string());
    }
    if !record.cache.nocache_output_identical {
        failures.push(
            "a --no-cache run disagreed with the cached one: the cache is serving results \
             computed under different inputs"
                .to_string(),
        );
    }

    // Losing lint coverage for a language poly claims is a regression in the
    // engine, not a property of the corpus.
    for (reason, count) in &record.lint.skips_by_reason {
        let Some(language) = reason.strip_prefix("no lint rules for ") else {
            continue;
        };
        if LINT_CLAIMED_LANGUAGES.contains(&language) {
            failures.push(format!(
                "{count} file(s) reported `no lint rules for {language}`, but poly claims to lint it"
            ));
        }
    }

    for (phase, record) in [("lint", &record.lint), ("format", &record.fmt)] {
        if record.ceiling_ms > 0 && record.elapsed_ms > record.ceiling_ms {
            failures.push(format!(
                "{phase} took {}ms against a {}ms ceiling",
                record.elapsed_ms, record.ceiling_ms
            ));
        }
    }

    failures
}

/// A few names rather than all of them: the point is to identify the shape of
/// the failure, and the full list is in the record.
fn sample(paths: &[String]) -> String {
    const MAX: usize = 5;
    let shown = paths.iter().take(MAX).cloned().collect::<Vec<_>>().join(", ");
    if paths.len() > MAX {
        format!("{shown}, and {} more", paths.len() - MAX)
    } else {
        shown
    }
}
