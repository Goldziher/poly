//! Autofix edit application: committing a diagnostic's `fix` edits to a file's
//! contents.
//!
//! Split out of `runner.rs` so the runner keeps to the pipeline itself
//! (discover -> cache -> engine -> report) and edit arithmetic stays one concern
//! per file.

use crate::engine::Edit;

/// Apply autofix edit groups to `content`, one group per diagnostic.
///
/// Each group is the full `fix` vec of one [`Diagnostic`](crate::engine::Diagnostic) and is applied
/// **atomically**: all of its edits apply, or none do.
///
/// Selection rules (right-to-left):
/// 1. Any group whose own edits overlap each other internally is discarded
///    (prevents corrupted output from a malformed backend fix).
/// 2. Groups are attempted rightmost-first.  If any edit in a group would
///    reach into bytes already committed by a previously-applied group, the
///    entire group is skipped; the convergence loop in `lint_one` will retry
///    it on the next pass once those diagnostics have been re-evaluated.
///
/// Returns the rewritten text and how many groups were committed, or `None` if
/// no edit was applied. The count is what lets the report say how many issues a
/// `--fix` run resolved rather than only how many survived it.
pub(super) fn apply_edits(content: &str, edit_groups: &[&[Edit]]) -> Option<(String, usize)> {
    let mut valid: Vec<&[Edit]> = edit_groups
        .iter()
        .copied()
        .filter(|g| !g.is_empty() && !has_internal_overlap(g))
        .collect();
    valid.sort_by_key(|g| std::cmp::Reverse(g.iter().map(|e| e.end_byte).max().unwrap_or(0)));

    let mut result = content.to_string();
    let mut prev_start = usize::MAX;
    let mut applied = 0usize;

    'groups: for group in &valid {
        for e in *group {
            if e.start_byte > e.end_byte || e.end_byte > result.len() || e.end_byte > prev_start {
                continue 'groups;
            }
            if !result.is_char_boundary(e.start_byte) || !result.is_char_boundary(e.end_byte) {
                continue 'groups;
            }
        }

        if let [e] = *group {
            result.replace_range(e.start_byte..e.end_byte, &e.replacement);
        } else {
            let mut ordered: Vec<&Edit> = group.iter().collect();
            ordered.sort_by_key(|e| std::cmp::Reverse(e.start_byte));
            for e in &ordered {
                result.replace_range(e.start_byte..e.end_byte, &e.replacement);
            }
        }

        prev_start = group.iter().map(|e| e.start_byte).min().unwrap_or(prev_start);
        applied += 1;
    }

    (applied > 0).then_some((result, applied))
}

/// Returns `true` when any two edits in `group` have overlapping byte ranges.
///
/// O(n²) — acceptable because fix groups are tiny (1–4 edits in practice).
fn has_internal_overlap(group: &[Edit]) -> bool {
    for (i, a) in group.iter().enumerate() {
        for b in group.iter().skip(i + 1) {
            let intersects = a.start_byte < b.end_byte && b.start_byte < a.end_byte;
            let same_point_insert =
                a.start_byte == a.end_byte && b.start_byte == b.end_byte && a.start_byte == b.start_byte;
            if intersects || same_point_insert {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(start: usize, end: usize, rep: &str) -> Edit {
        Edit {
            start_byte: start,
            end_byte: end,
            replacement: rep.to_owned(),
        }
    }
    /// Two diagnostics, each with two non-overlapping edits; all four apply.
    #[test]
    fn multi_edit_two_groups_apply_atomically() {
        let content = "hello world foo";
        let group_a = vec![edit(6, 11, "earth"), edit(12, 15, "bar")];
        let group_b = vec![edit(0, 5, "hey")];

        let (result, applied) =
            apply_edits(content, &[group_a.as_slice(), group_b.as_slice()]).expect("should produce output");
        assert_eq!(result, "hey earth bar");
        assert_eq!(applied, 2, "both diagnostics' fixes were committed");
    }

    /// A diagnostic whose edits overlap each other is skipped entirely.
    #[test]
    fn overlapping_edits_within_group_are_skipped() {
        let content = "abcdefgh";
        let bad_group = vec![edit(2, 6, "X"), edit(4, 8, "Y")];

        let result = apply_edits(content, &[bad_group.as_slice()]);
        assert!(result.is_none(), "overlapping group must produce no output");
    }

    /// When two groups from different diagnostics conflict, the leftward group
    /// is deferred (not corrupted).
    #[test]
    fn cross_group_conflict_defers_leftward_group() {
        let content = "abcde";
        let group_a = vec![edit(3, 5, "XX")];
        let group_b = vec![edit(2, 4, "YY")];

        let (result, applied) = apply_edits(content, &[group_a.as_slice(), group_b.as_slice()])
            .expect("should produce output from group A");
        assert_eq!(result, "abcXX");
        assert_eq!(
            applied, 1,
            "the deferred group must not be counted as fixed — it is retried next pass"
        );
    }

    #[test]
    fn non_overlapping_edits_pass_internal_check() {
        let group = vec![edit(0, 5, "a"), edit(5, 10, "b")];
        assert!(!has_internal_overlap(&group));
    }

    #[test]
    fn adjacent_edits_are_not_overlapping() {
        let group = vec![edit(0, 5, "a"), edit(5, 10, "b")];
        assert!(!has_internal_overlap(&group));
    }

    #[test]
    fn touching_edits_with_overlap_detected() {
        let group = vec![edit(0, 6, "a"), edit(4, 10, "b")];
        assert!(has_internal_overlap(&group));
    }
}
