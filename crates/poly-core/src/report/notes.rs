//! The qualification notes that keep a summary honest: what discovery excluded
//! or could not identify, and what the run skipped — as the summary's breakdown
//! lines, as the follow-on detail block, and as the stderr echoes the `json` /
//! `toon` formats use.

use std::fmt::Write as _;

use super::layout::{self, directories, files, paths, wrap_items};
use super::shared::{Verbosity, strip_ansi};
use super::theme::Theme;
use crate::discover::DiscoveryReport;
use crate::runner::{FormatResult, SkippedFile};

/// Indent of a summary breakdown line, and of the detail block beneath it.
const INDENT: &str = "  ";

/// Indent of a continuation (`e.g.` / wrapped) line, one step further in than
/// the line it continues.
const CONTINUATION: &str = "    ";

/// The summary's breakdown line naming what `[discovery] exclude` / `--exclude`
/// pruned, or `None` when discovery excluded nothing.
///
/// Files and directories are reported apart because only the file count is
/// exact: an excluded directory is pruned at its boundary and never descended
/// into, so the number of files inside it was never measured. Collapsing the two
/// into one "N excluded" number would claim a precision nobody paid for, which
/// is the same dishonesty as the unqualified pass this whole feature exists to
/// remove.
pub(super) fn exclusion_clause(discovery: &DiscoveryReport) -> Option<String> {
    match (discovery.excluded_files, discovery.excluded_directories) {
        (0, 0) => None,
        (count, 0) => Some(format!("{} excluded by config", files(count))),
        (0, count) => Some(format!("{} excluded by config", directories(count))),
        (file_count, directory_count) => Some(format!(
            "{} and {} excluded by config",
            files(file_count),
            directories(directory_count)
        )),
    }
}

/// The summary's breakdown line naming the files discovery could not identify as
/// any language, or `None` when every walked file was identified.
///
/// Separate from [`exclusion_clause`] because nothing excluded these: they are
/// files poly has no idea how to read. Kept out of the skipped set (see
/// [`DiscoveryReport::unrecognized_files`]) but not out of the summary — a run
/// that walked a directory and understood two thirds of it must not report the
/// two thirds as if they were the whole.
pub(super) fn unrecognized_clause(discovery: &DiscoveryReport) -> Option<String> {
    (discovery.unrecognized_files > 0).then(|| {
        format!(
            "{} of unrecognized type not checked",
            files(discovery.unrecognized_files)
        )
    })
}

/// The summary's breakdown line naming what the built-in vendored/generated
/// prune set removed, or `None` when it pruned nothing.
///
/// Kept apart from [`exclusion_clause`] because nothing the user wrote caused
/// it: these are poly's own heuristics, and a reader who sees a directory they
/// did not expect to lose needs to know which of the two to go and change.
pub(super) fn pruned_clause(discovery: &DiscoveryReport) -> Option<String> {
    (discovery.pruned_directories > 0).then(|| {
        format!(
            "{} skipped by the built-in prune set",
            directories(discovery.pruned_directories)
        )
    })
}

/// How many exclude rules the detail line names before summarising the rest.
///
/// Rules are ordered by how much they pruned, so the ones worth investigating
/// come first; a repo with twenty excludes should not turn every clean run into
/// a wall of text.
const MAX_LISTED_EXCLUDE_RULES: usize = 5;

/// Render the follow-on detail lines for an exclusion: which rules matched, what
/// each pruned, and the caveat that excluded directories were never walked.
///
/// Returns `None` when discovery excluded nothing, so a clean run stays quiet —
/// and `None` under [`Verbosity::Quiet`], which asks for the summary without the
/// itemisation beneath it. The counts themselves are in the summary at every
/// level, so nothing is hidden either way.
pub fn render_discovery_note(discovery: &DiscoveryReport, verbosity: Verbosity) -> Option<String> {
    if !discovery.has_notes() || !verbosity.shows_notes() {
        return None;
    }
    let mut out = String::new();
    if discovery.is_empty() {
        // Nothing was excluded; the only thing to report is what could not be
        // identified, appended by the tail below.
    } else if discovery.rules.is_empty() {
        let _ = writeln!(out, "{INDENT}excluded from discovery by an exclude rule");
    } else {
        let mut rules: Vec<String> = discovery
            .rules
            .iter()
            .take(MAX_LISTED_EXCLUDE_RULES)
            .map(|rule| {
                let mut counts: Vec<String> = Vec::with_capacity(2);
                if rule.files > 0 {
                    counts.push(files(rule.files));
                }
                if rule.directories > 0 {
                    counts.push(directories(rule.directories));
                }
                format!("{} ({})", rule.pattern, counts.join(", "))
            })
            .collect();
        if let Some(rest) = discovery
            .rules
            .len()
            .checked_sub(MAX_LISTED_EXCLUDE_RULES)
            .filter(|n| *n > 0)
        {
            rules.push(format!("and {} more", layout::rules(rest)));
        }
        let _ = writeln!(
            out,
            "{}",
            wrap_items(
                &format!("{INDENT}excluded by [discovery] exclude / --exclude: "),
                &rules,
                CONTINUATION.len(),
            )
        );
    }
    if discovery.excluded_directories > 0 {
        let _ = writeln!(
            out,
            "{INDENT}excluded directories were not walked, so the files inside them are not counted"
        );
    }
    if discovery.pruned_directories > 0 {
        // Named, not merely counted: the whole failure this reports — a tracked
        // `build/` of first-party source silently dropped — is invisible in a
        // bare number and obvious in a path.
        let _ = writeln!(
            out,
            "{INDENT}{} skipped by the built-in prune set",
            directories(discovery.pruned_directories)
        );
        let names: Vec<String> = discovery
            .pruned_samples
            .iter()
            .map(|sample| sample.path.display().to_string())
            .collect();
        if let Some(line) = layout::fit_samples(&format!("{CONTINUATION}e.g. "), &names) {
            let _ = writeln!(out, "{line}");
        }
        let _ = writeln!(
            out,
            "{INDENT}these were not walked, so the files inside them are not counted; \
             keep one with [discovery] no_prune"
        );
    }
    if discovery.excluded_explicit > 0 {
        let _ = writeln!(
            out,
            "{INDENT}{} named on the command line matched exclusions (use --include-excluded to check them)",
            paths(discovery.excluded_explicit)
        );
    }
    if discovery.unrecognized_files > 0 {
        // Named, not merely counted: "4 unrecognized" reads as an oversight
        // until you can see that they are PNGs. A caller who disagrees — a
        // `.kt`-like file poly should have identified — can only tell from the
        // names.
        let _ = writeln!(
            out,
            "{INDENT}{} not identified as any language, so no engine saw them",
            files(discovery.unrecognized_files)
        );
        let samples: Vec<String> = discovery
            .unrecognized_samples
            .iter()
            .map(|path| path.display().to_string())
            .collect();
        if let Some(line) = layout::fit_samples(&format!("{CONTINUATION}e.g. "), &samples) {
            let _ = writeln!(out, "{line}");
        }
    }
    Some(Theme::STDOUT.skipped(out))
}

/// Append [`render_discovery_note`] to `out`, if there is anything to say.
pub(super) fn push_discovery_note(out: &mut String, discovery: &DiscoveryReport, verbosity: Verbosity) {
    if let Some(note) = render_discovery_note(discovery, verbosity) {
        out.push_str(&note);
    }
}

/// Print the discovery note to **stderr**, for the `json`/`toon` formats.
///
/// Under those formats stdout must stay a single valid document, so the
/// qualification goes to stderr — the same split the whole-project lint phase
/// already uses. Colour is resolved against stderr rather than stdout, because
/// that is the stream it lands on: a piped stdout with a TTY stderr (the usual
/// `poly lint --format json > out.json`) would otherwise lose the highlight.
pub fn eprint_discovery_note(discovery: &DiscoveryReport, verbosity: Verbosity) {
    let Some(note) = render_discovery_note(discovery, verbosity) else {
        return;
    };
    // `render_discovery_note` resolves colour for stdout; strip that and re-apply
    // for stderr so the two streams cannot disagree.
    eprint!("{}", Theme::STDERR.skipped(strip_ansi(&note)));
}

/// How many files one skip *reason* names before the note collapses it to a
/// count plus a sample.
///
/// The bound is per reason, not over the list as a whole. A flat cap looks the
/// same until one reason dominates: poly's own repository emitted 229
/// consecutive `no lint rules for Rust` lines, which pushed every *other* reason
/// past the cap — so the rare skip that actually warranted attention was exactly
/// the one dropped. Grouping bounds each reason independently, so a bulk reason
/// costs two lines however many files it covers and can never crowd out a
/// one-off. Groups at or under the bound keep the per-file form: naming three
/// files says more than counting them. `--verbose` lists every file, and
/// `--format json` always carries the complete set.
const MAX_NAMED_SKIPS_PER_REASON: usize = 3;

/// The per-file skips carried by a results slice, as run-level [`SkippedFile`]s.
///
/// Lets the results-only renderers produce the same summary as the run-level
/// ones, which additionally know about explicitly named paths that no engine
/// covers (those never reach a [`FormatResult`] at all).
pub(super) fn skips_from_results(results: &[FormatResult]) -> Vec<SkippedFile> {
    results
        .iter()
        .filter_map(|r| {
            r.skipped.as_ref().map(|reason| SkippedFile {
                path: r.path.clone(),
                reason: reason.clone(),
            })
        })
        .collect()
}

/// The summary's breakdown line naming what the run skipped and why, or `None`
/// when it skipped nothing — so the common path gains no new text.
///
/// Wrapped to the terminal, because a repository with a dozen distinct skip
/// reasons produced a 300-character line that an 80-column terminal shredded
/// mid-path.
pub(super) fn skipped_clause(skipped: &[SkippedFile]) -> Option<String> {
    if skipped.is_empty() {
        return None;
    }
    let reasons = skip_reason_counts(skipped);
    Some(wrap_items(
        &format!("{INDENT}{} skipped: ", files(skipped.len())),
        &reasons,
        INDENT.len() + CONTINUATION.len(),
    ))
}

/// The indented breakdown printed under a summary headline: what was inspected,
/// and every reason the file set was smaller than the tree.
///
/// One builder for both `lint` and `format`, which had drifted into four copies
/// of the same four clauses. `verb` is what happened to the checked files —
/// `linted` or `checked` — and is the only difference between the two callers.
///
/// The lines carry their own indent and colour so a caller only has to print
/// them. Every count here is reported at **every** verbosity including
/// `--quiet`: what `--quiet` drops is the itemisation beneath, never the fact
/// that files went unexamined.
pub(super) fn qualification_lines(
    checked: Option<usize>,
    verb: &str,
    skipped: &[SkippedFile],
    discovery: &DiscoveryReport,
) -> Vec<String> {
    let theme = Theme::STDOUT;
    let mut lines: Vec<String> = Vec::with_capacity(5);
    if let Some(checked) = checked {
        lines.push(theme.secondary(format!("{INDENT}{} {verb}", files(checked))));
    }
    if let Some(clause) = skipped_clause(skipped) {
        lines.push(theme.skipped(clause));
    }
    for clause in [
        exclusion_clause(discovery),
        pruned_clause(discovery),
        unrecognized_clause(discovery),
    ]
    .into_iter()
    .flatten()
    {
        lines.push(theme.skipped(format!("{INDENT}{clause}")));
    }
    lines
}

/// Render the follow-on detail lines naming what the run skipped, grouped by
/// reason.
///
/// Returns `None` when nothing was skipped. A count without names is what forced
/// one consumer to reconstruct the expected skip set from a heuristic and parse
/// it back out of this very summary, so the names are the point; the reason
/// travels with each so a reader knows whether to fix the file, the config, or
/// their expectations. A reason covering more than
/// `MAX_NAMED_SKIPS_PER_REASON` files collapses to a count and a sample;
/// [`Verbosity::Verbose`] lists every file individually, and [`Verbosity::Quiet`]
/// drops the block — the summary's count and reasons stay, so nothing is hidden,
/// only un-itemised.
///
/// There is deliberately no cap on the *number of reasons*: reasons are bounded
/// by the languages and decline conditions actually present, each costs at most
/// two lines, and a cap there would reintroduce the very failure this grouping
/// removes — a rare reason silently dropped because a common one filled the
/// quota.
pub fn render_skip_note(skipped: &[SkippedFile], verbosity: Verbosity) -> Option<String> {
    if skipped.is_empty() || !verbosity.shows_notes() {
        return None;
    }
    let mut out = String::new();
    if verbosity.shows_finding_detail() {
        for entry in skipped {
            let _ = writeln!(out, "{INDENT}skipped {}: {}", entry.path.display(), entry.reason);
        }
        return Some(Theme::STDOUT.skipped(out));
    }
    let mut collapsed = false;
    for (reason, group) in group_skips_by_reason(skipped) {
        if group.len() <= MAX_NAMED_SKIPS_PER_REASON {
            for path in group {
                let _ = writeln!(out, "{INDENT}skipped {}: {reason}", path.display());
            }
            continue;
        }
        collapsed = true;
        let _ = writeln!(out, "{INDENT}skipped {}: {reason}", files(group.len()));
        let samples: Vec<String> = group
            .iter()
            .take(MAX_NAMED_SKIPS_PER_REASON)
            .map(|path| path.display().to_string())
            .collect();
        if let Some(line) = layout::fit_samples(&format!("{CONTINUATION}e.g. "), &samples) {
            let _ = writeln!(out, "{line}");
        }
    }
    // Said once, at the end of the block. Repeating it under every collapsed
    // reason printed the same sentence twelve times in one run over Kubernetes,
    // which is how a helpful hint turns into noise the reader learns to skip.
    if collapsed {
        let _ = writeln!(
            out,
            "{INDENT}pass --verbose to list every skipped file, or --format json for the full set"
        );
    }
    Some(Theme::STDOUT.skipped(out))
}

/// The skipped files bucketed by reason, most files first with ties broken by
/// reason text.
///
/// The same ordering [`skip_reason_counts`] uses, so the summary's breakdown and
/// the detail lines beneath it name the reasons in the same sequence — a reader
/// matching one against the other should not have to search.
fn group_skips_by_reason(skipped: &[SkippedFile]) -> Vec<(&str, Vec<&std::path::Path>)> {
    let mut groups: Vec<(&str, Vec<&std::path::Path>)> = Vec::new();
    for entry in skipped {
        match groups.iter_mut().find(|(reason, _)| *reason == entry.reason) {
            Some((_, paths)) => paths.push(entry.path.as_path()),
            None => groups.push((entry.reason.as_str(), vec![entry.path.as_path()])),
        }
    }
    groups.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(b.0)));
    groups
}

/// Append [`render_skip_note`] to `out`, if there is anything to say.
pub(super) fn push_skip_note(out: &mut String, skipped: &[SkippedFile], verbosity: Verbosity) {
    if let Some(note) = render_skip_note(skipped, verbosity) {
        out.push_str(&note);
    }
}

/// Print the skip note to **stderr**, for the `json`/`toon` formats.
///
/// Those formats already carry the skipped set structurally on stdout; this is
/// the human-visible echo, on the stream that cannot corrupt the document — the
/// same split [`eprint_discovery_note`] uses.
pub fn eprint_skip_note(skipped: &[SkippedFile], verbosity: Verbosity) {
    let Some(note) = render_skip_note(skipped, verbosity) else {
        return;
    };
    eprint!("{}", Theme::STDERR.skipped(strip_ansi(&note)));
}

/// The distinct skip reasons across `skipped`, most frequent first, each with
/// its count — so the summary names *why* files were skipped rather than only
/// how many.
fn skip_reason_counts(skipped: &[SkippedFile]) -> Vec<String> {
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for reason in skipped.iter().map(|s| s.reason.as_str()) {
        match counts.iter_mut().find(|(name, _)| *name == reason) {
            Some((_, count)) => *count += 1,
            None => counts.push((reason, 1)),
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    // With a single reason the outer "N skipped" already carries the count, so
    // repeating it reads as "1 skipped (1 …)".
    if let [(reason, _)] = counts.as_slice() {
        return vec![(*reason).to_owned()];
    }
    counts
        .iter()
        .map(|(reason, count)| format!("{} {reason}", layout::number(*count)))
        .collect()
}
