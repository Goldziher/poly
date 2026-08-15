//! The qualification notes that keep a summary honest: what discovery excluded
//! or could not identify, and what the run skipped — as summary clauses, as the
//! follow-on detail lines, and as the stderr echoes the `json` / `toon` formats
//! use.

use std::fmt::Write as _;

use owo_colors::{OwoColorize, Stream::Stderr, Stream::Stdout};

use super::shared::strip_ansi;
use crate::discover::DiscoveryReport;
use crate::runner::{FormatResult, SkippedFile};

/// The summary clause naming what `[discovery] exclude` / `--exclude` pruned, or
/// `None` when discovery excluded nothing.
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
        (files, 0) => Some(format!("{files} file(s) excluded by config")),
        (0, directories) => Some(format!("{directories} director(ies) excluded by config")),
        (files, directories) => Some(format!(
            "{files} file(s) and {directories} director(ies) excluded by config"
        )),
    }
}

/// The summary clause naming the files discovery could not identify as any
/// language, or `None` when every walked file was identified.
///
/// Separate from [`exclusion_clause`] because nothing excluded these: they are
/// files poly has no idea how to read. Kept out of the skipped set (see
/// [`DiscoveryReport::unrecognized_files`]) but not out of the summary — a run
/// that walked a directory and understood two thirds of it must not report the
/// two thirds as if they were the whole.
pub(super) fn unrecognized_clause(discovery: &DiscoveryReport) -> Option<String> {
    (discovery.unrecognized_files > 0).then(|| {
        format!(
            "{} file(s) of unrecognized type not checked",
            discovery.unrecognized_files
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
/// Returns `None` when discovery excluded nothing, so a clean run stays quiet.
/// Every line is indented two spaces to read as a continuation of the summary.
pub fn render_discovery_note(discovery: &DiscoveryReport) -> Option<String> {
    if !discovery.has_notes() {
        return None;
    }
    let mut out = String::new();
    if discovery.is_empty() {
        // Nothing was excluded; the only thing to report is what could not be
        // identified, appended by the tail below.
    } else if discovery.rules.is_empty() {
        let _ = writeln!(out, "  excluded from discovery by an exclude rule");
    } else {
        let mut rules = discovery
            .rules
            .iter()
            .take(MAX_LISTED_EXCLUDE_RULES)
            .map(|rule| {
                let mut counts: Vec<String> = Vec::with_capacity(2);
                if rule.files > 0 {
                    counts.push(format!("{} file(s)", rule.files));
                }
                if rule.directories > 0 {
                    counts.push(format!("{} dir(s)", rule.directories));
                }
                format!("{} ({})", rule.pattern, counts.join(", "))
            })
            .collect::<Vec<_>>()
            .join(", ");
        if let Some(rest) = discovery
            .rules
            .len()
            .checked_sub(MAX_LISTED_EXCLUDE_RULES)
            .filter(|n| *n > 0)
        {
            let _ = write!(rules, ", and {rest} more rule(s)");
        }
        let _ = writeln!(out, "  excluded by [discovery] exclude / --exclude: {rules}");
    }
    if discovery.excluded_directories > 0 {
        let _ = writeln!(
            out,
            "  excluded directories were not walked, so the files inside them are not counted"
        );
    }
    if discovery.excluded_explicit > 0 {
        let _ = writeln!(
            out,
            "  {} path(s) named on the command line matched exclusions (use --include-excluded to check them)",
            discovery.excluded_explicit
        );
    }
    if discovery.unrecognized_files > 0 {
        // Named, not merely counted: "4 unrecognized" reads as an oversight
        // until you can see that they are PNGs. A caller who disagrees — a
        // `.kt`-like file poly should have identified — can only tell from the
        // names.
        let samples: Vec<String> = discovery
            .unrecognized_samples
            .iter()
            .map(|path| path.display().to_string())
            .collect();
        let _ = writeln!(
            out,
            "  {} file(s) were not identified as any language and no engine saw them{}",
            discovery.unrecognized_files,
            if samples.is_empty() {
                String::new()
            } else {
                format!(" (e.g. {})", samples.join(", "))
            }
        );
    }
    Some(out.if_supports_color(Stdout, |t| t.yellow()).to_string())
}

/// Append [`render_discovery_note`] to `out`, if there is anything to say.
pub(super) fn push_discovery_note(out: &mut String, discovery: &DiscoveryReport) {
    if let Some(note) = render_discovery_note(discovery) {
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
pub fn eprint_discovery_note(discovery: &DiscoveryReport) {
    if !discovery.has_notes() {
        return;
    }
    let Some(note) = render_discovery_note(discovery) else {
        return;
    };
    // `render_discovery_note` resolves colour for stdout; strip that and re-apply
    // for stderr so the two streams cannot disagree.
    let plain = strip_ansi(&note);
    eprint!("{}", plain.if_supports_color(Stderr, |t| t.yellow()));
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

/// The summary clause naming what the run skipped, or `None` when it skipped
/// nothing — so the common path gains no new text.
pub(super) fn skipped_clause(skipped: &[SkippedFile]) -> Option<String> {
    (!skipped.is_empty()).then(|| format!("{} skipped ({})", skipped.len(), skip_reason_summary(skipped)))
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
/// `verbose` lists every file individually.
///
/// There is deliberately no cap on the *number of reasons*: reasons are bounded
/// by the languages and decline conditions actually present, each costs at most
/// two lines, and a cap there would reintroduce the very failure this grouping
/// removes — a rare reason silently dropped because a common one filled the
/// quota.
pub fn render_skip_note(skipped: &[SkippedFile], verbose: bool) -> Option<String> {
    if skipped.is_empty() {
        return None;
    }
    let mut out = String::new();
    if verbose {
        for entry in skipped {
            let _ = writeln!(out, "  skipped {}: {}", entry.path.display(), entry.reason);
        }
    } else {
        for (reason, paths) in group_skips_by_reason(skipped) {
            if paths.len() <= MAX_NAMED_SKIPS_PER_REASON {
                for path in paths {
                    let _ = writeln!(out, "  skipped {}: {reason}", path.display());
                }
                continue;
            }
            let samples: Vec<String> = paths
                .iter()
                .take(MAX_NAMED_SKIPS_PER_REASON)
                .map(|path| path.display().to_string())
                .collect();
            let _ = writeln!(out, "  skipped {} file(s): {reason}", paths.len());
            let _ = writeln!(
                out,
                "    e.g. {} — pass --verbose to list them, or --format json for the full set",
                samples.join(", ")
            );
        }
    }
    Some(out.if_supports_color(Stdout, |t| t.yellow()).to_string())
}

/// The skipped files bucketed by reason, most files first with ties broken by
/// reason text.
///
/// The same ordering [`skip_reason_summary`] uses, so the headline clause and
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
pub(super) fn push_skip_note(out: &mut String, skipped: &[SkippedFile], verbose: bool) {
    if let Some(note) = render_skip_note(skipped, verbose) {
        out.push_str(&note);
    }
}

/// Print the skip note to **stderr**, for the `json`/`toon` formats.
///
/// Those formats already carry the skipped set structurally on stdout; this is
/// the human-visible echo, on the stream that cannot corrupt the document — the
/// same split [`eprint_discovery_note`] uses.
pub fn eprint_skip_note(skipped: &[SkippedFile], verbose: bool) {
    let Some(note) = render_skip_note(skipped, verbose) else {
        return;
    };
    let plain = strip_ansi(&note);
    eprint!("{}", plain.if_supports_color(Stderr, |t| t.yellow()));
}

/// Summarise the distinct skip reasons across `skipped`, most frequent first, so
/// the summary names *why* files were skipped rather than only how many.
fn skip_reason_summary(skipped: &[SkippedFile]) -> String {
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
        return (*reason).to_owned();
    }
    counts
        .iter()
        .map(|(reason, count)| format!("{count} {reason}"))
        .collect::<Vec<_>>()
        .join(", ")
}
