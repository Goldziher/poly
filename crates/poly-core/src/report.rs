//! Output rendering in three formats: `pretty` (colored, human-oriented),
//! `json` (`serde_json`), and `toon` (Token-Oriented Object Notation).
//!
//! Coloring goes through owo-colors' `if_supports_color`, which respects both
//! TTY detection and the global override set by `--no-color`. The `toon`
//! renderers fall back to JSON if TOON serialization fails so output is never
//! lost. The `pretty` renderers split into a `render_*` core that produces the
//! string and a `report_*` wrapper that prints it, so the rendered text can be
//! snapshot-tested.
//!
//! ## Verbosity contract
//!
//! [`Verbosity`] selects how much of each diagnostic the `pretty` renderers
//! show:
//! - **default** — one terse line per finding (`level  engine  code?  line:col?
//!   title`). `description`, `url`, and `metadata` are hidden.
//! - **`--verbose`** — additionally renders `description`, `url`, and any
//!   `metadata` as indented lines, and lifts the cap on the skipped-file note.
//! - **`--debug`** — additionally renders a dim per-file debug block (engine
//!   version, cache hit/miss, timing).
//!
//! For `json` / `toon` the full structured record is **always** emitted (serde
//! omits empty/`None` fields), so `--verbose` is a no-op there; `--debug` simply
//! causes the runner to attach the `debug` field, which then serializes.

use std::fmt::Write as _;

use owo_colors::{OwoColorize, Stream::Stderr, Stream::Stdout};

use crate::discover::DiscoveryReport;
use crate::engine::Severity;
use crate::runner::{FormatError, FormatResult, FormatRun, LintError, LintResult, LintRun, RunDebug, SkippedFile};

/// How much detail the human-oriented (`pretty`) renderers emit. `Copy` so it
/// threads cheaply through the renderers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Verbosity {
    /// Show `description`, `url`, and `metadata` for each finding.
    pub verbose: bool,
    /// Show the per-file debug block (engine version, cache hit/miss, timing).
    pub debug: bool,
}

impl Verbosity {
    /// Construct a [`Verbosity`] from the two flags.
    pub fn new(verbose: bool, debug: bool) -> Self {
        Self { verbose, debug }
    }
}

/// Format the colored severity label for a diagnostic.
fn severity_label(severity: Severity) -> String {
    match severity {
        Severity::Error => "error".if_supports_color(Stdout, |t| t.red()).to_string(),
        Severity::Warning => "warning".if_supports_color(Stdout, |t| t.yellow()).to_string(),
        Severity::Info => "info".if_supports_color(Stdout, |t| t.blue()).to_string(),
        Severity::Hint => "hint".if_supports_color(Stdout, |t| t.cyan()).to_string(),
    }
}

/// Render the dim per-file debug block (engine version, cache hit/miss, timing).
fn render_debug_block(out: &mut String, debug: &RunDebug) {
    for e in &debug.engines {
        let status = if e.cache_hit { "cache hit" } else { "ran" };
        let line = format!(
            "[debug] {} v{}  {}  {:.2}ms",
            e.engine, e.version, status, e.duration_ms
        );
        let _ = writeln!(out, "      {}", line.if_supports_color(Stdout, |t| t.dimmed()));
    }
}

/// The summary clause naming what `[discovery] exclude` / `--exclude` pruned, or
/// `None` when discovery excluded nothing.
///
/// Files and directories are reported apart because only the file count is
/// exact: an excluded directory is pruned at its boundary and never descended
/// into, so the number of files inside it was never measured. Collapsing the two
/// into one "N excluded" number would claim a precision nobody paid for, which
/// is the same dishonesty as the unqualified pass this whole feature exists to
/// remove.
fn exclusion_clause(discovery: &DiscoveryReport) -> Option<String> {
    match (discovery.excluded_files, discovery.excluded_directories) {
        (0, 0) => None,
        (files, 0) => Some(format!("{files} file(s) excluded by config")),
        (0, directories) => Some(format!("{directories} director(ies) excluded by config")),
        (files, directories) => Some(format!(
            "{files} file(s) and {directories} director(ies) excluded by config"
        )),
    }
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
    if discovery.is_empty() {
        return None;
    }
    let mut out = String::new();
    if discovery.rules.is_empty() {
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
            "  {} path(s) named on the command line were dropped by --force-exclude",
            discovery.excluded_explicit
        );
    }
    Some(out.if_supports_color(Stdout, |t| t.yellow()).to_string())
}

/// Append [`render_discovery_note`] to `out`, if there is anything to say.
fn push_discovery_note(out: &mut String, discovery: &DiscoveryReport) {
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
    if discovery.is_empty() {
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

/// Remove ANSI SGR sequences from `text`.
///
/// Only ever applied to poly's own rendered notes, which contain nothing more
/// exotic than `ESC [ … m`.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            out.push(ch);
            continue;
        }
        if chars.next() != Some('[') {
            continue;
        }
        for escape in chars.by_ref() {
            if escape == 'm' {
                break;
            }
        }
    }
    out
}

/// Build the human-oriented lint report as a string. By default one terse line
/// per diagnostic: `level  engine  code?  line:col?  title`. `--verbose` adds
/// `description`, `url`, and `metadata`; `--debug` adds a dim per-file debug
/// block. Returns the rendered text and the total diagnostic count.
///
/// The summary is unqualified: it can say "No issues found." but not how much was
/// looked at. Prefer [`render_lint_pretty_run`], which does both.
pub fn render_lint_pretty(results: &[LintResult], verbosity: Verbosity) -> (String, usize) {
    render_lint_core(results, None, &[], &[], &DiscoveryReport::default(), verbosity)
}

/// [`render_lint_pretty`] over a whole [`LintRun`], so the summary can state how
/// many files were linted, which ones it skipped, which ones failed, and what
/// discovery excluded before them.
pub fn render_lint_pretty_run(run: &LintRun, verbosity: Verbosity) -> (String, usize) {
    render_lint_core(
        &run.results,
        Some(run.checked),
        &run.skipped,
        &run.errors,
        &run.discovery,
        verbosity,
    )
}

/// Shared body of the lint renderers. `checked` is `None` when the caller has no
/// run-level count to report (the legacy results-only entry point).
fn render_lint_core(
    results: &[LintResult],
    checked: Option<usize>,
    skipped: &[SkippedFile],
    errors: &[LintError],
    discovery: &DiscoveryReport,
    verbosity: Verbosity,
) -> (String, usize) {
    let mut out = String::new();
    let mut total = 0usize;
    let mut fixable = 0usize;
    for r in results {
        if r.diagnostics.is_empty() {
            if verbosity.debug
                && let Some(debug) = &r.debug
            {
                let _ = writeln!(out, "{}", r.path.display().if_supports_color(Stdout, |t| t.bold()));
                render_debug_block(&mut out, debug);
            }
            continue;
        }
        let _ = writeln!(out, "{}", r.path.display().if_supports_color(Stdout, |t| t.bold()));
        for d in &r.diagnostics {
            total += 1;
            if !d.fix.is_empty() {
                fixable += 1;
            }
            let mut segments: Vec<String> = Vec::with_capacity(5);
            segments.push(severity_label(d.severity));
            segments.push(d.engine.if_supports_color(Stdout, |t| t.magenta()).to_string());
            if let Some(code) = d.code.as_deref() {
                segments.push(code.if_supports_color(Stdout, |t| t.dimmed()).to_string());
            }
            if let Some(span) = &d.span {
                let loc = format!("{}:{}", span.start_line, span.start_col);
                segments.push(loc.if_supports_color(Stdout, |t| t.dimmed()).to_string());
            }
            segments.push(d.title.clone());
            let _ = writeln!(out, "  {}", segments.join("  "));

            if verbosity.verbose {
                if let Some(description) = d.description.as_deref() {
                    let _ = writeln!(out, "      {}", description.if_supports_color(Stdout, |t| t.dimmed()));
                }
                if let Some(url) = d.url.as_deref() {
                    let _ = writeln!(out, "      {}", url.if_supports_color(Stdout, |t| t.dimmed()));
                }
                for (key, value) in &d.metadata {
                    let _ = writeln!(
                        out,
                        "      {}",
                        format!("{key}={value}").if_supports_color(Stdout, |t| t.dimmed()),
                    );
                }
            }
        }
        if verbosity.debug
            && let Some(debug) = &r.debug
        {
            render_debug_block(&mut out, debug);
        }
    }
    if total == 0 {
        // "No issues found." on its own cannot distinguish a verified-clean repo
        // from one whose every file was excluded — qualify it with what was
        // looked at and what was pruned.
        let mut tail: Vec<String> = Vec::with_capacity(3);
        if let Some(checked) = checked {
            tail.push(format!("{checked} file(s) linted"));
        }
        if let Some(clause) = skipped_clause(skipped) {
            tail.push(clause);
        }
        if let Some(clause) = exclusion_clause(discovery) {
            tail.push(clause);
        }
        // "No issues found" over an empty file set is a false reassurance
        // whether the files were excluded before the run or skipped during it —
        // in both cases nothing was examined, which is not the same as clean.
        let nothing_linted = checked == Some(0) && (!discovery.is_empty() || !skipped.is_empty());
        // A `--fix` run that resolved everything has no diagnostics left to
        // report, so "No issues found." was the summary of a run that had just
        // rewritten files: true about the end state, silent about the act.
        let fixed = fixed_clause(results);
        // A file whose engine errored was accepted for linting and then not
        // linted, so nothing here may read as a verdict on the repo — least of
        // all `Fixed N issue(s)`, which is true about what the run did and silent
        // about what it failed to do. The fix count still gets its own line
        // below, where it cannot stand in for a pass.
        let headline = match (!errors.is_empty(), nothing_linted, &fixed) {
            (true, _, _) => "Lint did not complete."
                .if_supports_color(Stdout, |t| t.red())
                .to_string(),
            (false, true, _) => "Nothing was linted."
                .if_supports_color(Stdout, |t| t.yellow())
                .to_string(),
            (false, false, Some(fixed)) => fixed.if_supports_color(Stdout, |t| t.green()).to_string(),
            (false, false, None) => "No issues found.".if_supports_color(Stdout, |t| t.green()).to_string(),
        };
        if tail.is_empty() {
            let _ = writeln!(out, "{headline}");
        } else {
            let _ = writeln!(out, "{headline} ({})", tail.join(", "));
        }
        if !errors.is_empty()
            && let Some(fixed) = &fixed
        {
            let _ = writeln!(out, "{}", fixed.if_supports_color(Stdout, |t| t.yellow()));
        }
    } else {
        let mut headline = format!("{total} issue(s) found.");
        let mut tail: Vec<String> = Vec::with_capacity(3);
        if let Some(checked) = checked {
            tail.push(format!("{checked} file(s) linted"));
        }
        if let Some(clause) = skipped_clause(skipped) {
            tail.push(clause);
        }
        if let Some(clause) = exclusion_clause(discovery) {
            tail.push(clause);
        }
        if !tail.is_empty() {
            let _ = write!(headline, " ({})", tail.join(", "));
        }
        let _ = writeln!(out, "\n{}", headline.if_supports_color(Stdout, |t| t.red()));
        // A partially fixed run reports both halves: what it changed under the
        // caller, and what it left for them. Never green once a file errored —
        // the run is incomplete whatever it managed to fix.
        if let Some(fixed) = fixed_clause(results) {
            let colored = if errors.is_empty() {
                fixed.if_supports_color(Stdout, |t| t.green()).to_string()
            } else {
                fixed.if_supports_color(Stdout, |t| t.yellow()).to_string()
            };
            let _ = writeln!(out, "{colored}");
        }
        if fixable > 0 {
            let _ = writeln!(
                out,
                "{}",
                format!("{fixable} fixable with the `--fix` option.").if_supports_color(Stdout, |t| t.green())
            );
        }
    }
    // What was never discovered is as load-bearing as what was: a finding list
    // is only as trustworthy as the file set behind it.
    push_discovery_note(&mut out, discovery);
    // Same argument one step later in the pipeline: a file that was discovered
    // but never inspected is not evidence of anything, so it is named.
    push_skip_note(&mut out, skipped, verbosity.verbose);
    // Withholding a fix must be visible: an invisible skip is the same failure as
    // an invisible fix, just in the other direction.
    let withheld = results.iter().filter(|r| r.fix_withheld_generated).count();
    if withheld > 0 {
        let _ = writeln!(
            out,
            "{}",
            format!("{withheld} generated file(s) not fixed (pass `--fix-generated` to include them).")
                .if_supports_color(Stdout, |t| t.yellow())
        );
    }
    // Last, next to the exit code it drives: a file the linter could not process
    // was not checked, and the run must name it rather than leave it out.
    out.push_str(&render_lint_errors(errors));
    (out, total)
}

/// Render the files the linter could not process, naming each path.
///
/// A file whose engine errored has not been checked, so it must be visible and it
/// must not be folded into the skipped set — a skip is poly correctly declining a
/// file, an error is poly failing on one. The caller exits 2 on these, the same
/// code `poly fmt` uses for a formatter failure, and distinct from exit 1
/// ("findings remain").
pub fn render_lint_errors(errors: &[LintError]) -> String {
    if errors.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for error in errors {
        let _ = writeln!(
            out,
            "{} {}: {}",
            "error".if_supports_color(Stdout, |t| t.red()),
            error.path.display(),
            error.message
        );
    }
    let _ = writeln!(
        out,
        "{}",
        format!("{} file(s) could not be linted and were NOT checked.", errors.len())
            .if_supports_color(Stdout, |t| t.red())
    );
    out
}

/// Print the lint error block to **stderr**, for the `json`/`toon` formats.
///
/// Those formats carry the failures structurally on stdout; this is the
/// human-visible echo, on the stream that cannot corrupt the document — the same
/// split [`eprint_skip_note`] uses.
pub fn eprint_lint_errors(errors: &[LintError]) {
    if errors.is_empty() {
        return;
    }
    let plain = strip_ansi(&render_lint_errors(errors));
    eprint!("{}", plain.if_supports_color(Stderr, |t| t.red()));
}

/// Print the human-oriented lint report to stdout. Returns the total
/// diagnostic count.
pub fn report_lint_pretty(results: &[LintResult], verbosity: Verbosity) -> usize {
    let (text, total) = render_lint_pretty(results, verbosity);
    print!("{text}");
    total
}

/// Print the human-oriented lint report for a whole [`LintRun`] to stdout, so
/// the summary is qualified by what was linted and what was excluded. Returns
/// the total diagnostic count.
pub fn report_lint_pretty_run(run: &LintRun, verbosity: Verbosity) -> usize {
    let (text, total) = render_lint_pretty_run(run, verbosity);
    print!("{text}");
    total
}

/// Render lint results as pretty-printed JSON. The full structured record is
/// always emitted; serde omits `None`/empty fields. The `debug` field is present
/// only when the run collected it (`--debug`).
pub fn report_lint_json(results: &[LintResult]) -> String {
    serde_json::to_string_pretty(results).unwrap_or_else(|_| "[]".to_string())
}

/// Render lint results as TOON. Falls back to JSON if TOON serialization fails
/// so output is never silently dropped.
pub fn report_lint_toon(results: &[LintResult]) -> String {
    serde_toon::to_string(&results).unwrap_or_else(|_| report_lint_json(results))
}

/// The lint results of a run, with one entry appended per file the run skipped
/// and per file whose engine failed.
///
/// Neither produces diagnostics, so both are filtered out of
/// [`LintRun::results`] and used to be invisible to a machine consumer — which
/// left `--format json` unable to answer "what did you not look at?" and forced
/// one team to reconstruct the set from a heuristic and scrape the human
/// summary for it. The appended entries carry `path` plus `skipped` *or*
/// `error` — never both, since a file poly declined and a file poly failed on are
/// different outcomes — with an empty `diagnostics` list, so the document stays
/// the same array of per-file records and existing consumers are unaffected.
fn lint_results_for_output(run: &LintRun) -> Vec<LintResult> {
    let mut results = run.results.clone();
    let mut known: std::collections::BTreeSet<&std::path::Path> =
        run.results.iter().map(|r| r.path.as_path()).collect();
    let synthetic = |path: &std::path::Path, skipped: Option<String>, error: Option<String>| LintResult {
        path: path.to_path_buf(),
        diagnostics: Vec::new(),
        fix_withheld_generated: false,
        fixed: 0,
        skipped,
        error,
        debug: None,
    };
    // Errors first: a file that failed is reported as failed even if some other
    // stage also listed it, never downgraded to a skip.
    for error in &run.errors {
        if known.insert(error.path.as_path()) {
            results.push(synthetic(&error.path, None, Some(error.message.clone())));
        }
    }
    for entry in &run.skipped {
        if known.insert(entry.path.as_path()) {
            results.push(synthetic(&entry.path, Some(entry.reason.clone()), None));
        }
    }
    results
}

/// [`report_lint_json`] over a whole [`LintRun`], so the skipped and errored sets
/// are carried structurally rather than left to the human summary.
pub fn report_lint_json_run(run: &LintRun) -> String {
    report_lint_json(&lint_results_for_output(run))
}

/// [`report_lint_toon`] over a whole [`LintRun`], including the skipped and
/// errored sets.
pub fn report_lint_toon_run(run: &LintRun) -> String {
    report_lint_toon(&lint_results_for_output(run))
}

/// How many skipped files the note names before summarising the rest.
///
/// A run over a repo full of generated files can skip hundreds; naming every one
/// by default would bury the finding list it is meant to qualify. `--verbose`
/// lifts the cap, and `--format json` always carries the complete set.
const MAX_LISTED_SKIPS: usize = 20;

/// The per-file skips carried by a results slice, as run-level [`SkippedFile`]s.
///
/// Lets the results-only renderers produce the same summary as the run-level
/// ones, which additionally know about explicitly named paths that no engine
/// covers (those never reach a [`FormatResult`] at all).
fn skips_from_results(results: &[FormatResult]) -> Vec<SkippedFile> {
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
fn skipped_clause(skipped: &[SkippedFile]) -> Option<String> {
    (!skipped.is_empty()).then(|| format!("{} skipped ({})", skipped.len(), skip_reason_summary(skipped)))
}

/// Render the follow-on detail lines naming each skipped file and its reason.
///
/// Returns `None` when nothing was skipped. A count without names is what forced
/// one consumer to reconstruct the expected skip set from a heuristic and parse
/// it back out of this very summary, so the names are the point; the reason
/// travels with each so a reader knows whether to fix the file, the config, or
/// their expectations. Capped at `MAX_LISTED_SKIPS` entries unless `verbose`.
pub fn render_skip_note(skipped: &[SkippedFile], verbose: bool) -> Option<String> {
    if skipped.is_empty() {
        return None;
    }
    let limit = if verbose { skipped.len() } else { MAX_LISTED_SKIPS };
    let mut out = String::new();
    for entry in skipped.iter().take(limit) {
        let _ = writeln!(out, "  skipped {}: {}", entry.path.display(), entry.reason);
    }
    if let Some(rest) = skipped.len().checked_sub(limit).filter(|n| *n > 0) {
        let _ = writeln!(
            out,
            "  and {rest} more skipped file(s) — pass --verbose to list them, or --format json for the full set"
        );
    }
    Some(out.if_supports_color(Stdout, |t| t.yellow()).to_string())
}

/// Append [`render_skip_note`] to `out`, if there is anything to say.
fn push_skip_note(out: &mut String, skipped: &[SkippedFile], verbose: bool) {
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

/// The line reporting what `--fix` rewrote, or `None` when it rewrote nothing.
///
/// `--fix` reports the diagnostics that *remain*, so a run that fixed everything
/// printed `No issues found.` while rewriting files on disk — reporting on what
/// it found instead of on what it did. This states the second half.
fn fixed_clause(results: &[LintResult]) -> Option<String> {
    let issues: usize = results.iter().map(|r| r.fixed).sum();
    if issues == 0 {
        return None;
    }
    let files = results.iter().filter(|r| r.fixed > 0).count();
    Some(format!("Fixed {issues} issue(s) in {files} file(s)."))
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

/// Build the human-oriented format report as a string. `check` selects
/// "would reformat" vs "reformatted" phrasing. `--debug` appends a dim per-file
/// debug block (engine version, cache hit/miss, timing). Returns the rendered
/// text and the number of changed files.
pub fn render_format_pretty(results: &[FormatResult], check: bool, verbosity: Verbosity) -> (String, usize) {
    render_format_core(
        results,
        &skips_from_results(results),
        &DiscoveryReport::default(),
        check,
        verbosity,
    )
}

/// [`render_format_pretty`] over a whole [`FormatRun`], so the summary can say
/// what it skipped and what discovery excluded before the checked files were
/// reached.
pub fn render_format_pretty_run(run: &FormatRun, check: bool, verbosity: Verbosity) -> (String, usize) {
    let (mut out, changed) = render_format_core(&run.results, &run.skipped, &run.discovery, check, verbosity);
    out.push_str(&render_format_errors(&run.errors));
    (out, changed)
}

/// Render the files the formatter could not process, naming each path.
///
/// A file whose engine errored has not been verified, so it must be visible and
/// it must not be folded into the pass/fail count for *formatted* files — the
/// caller exits 2 on these, distinct from exit 1 ("files changed").
pub fn render_format_errors(errors: &[FormatError]) -> String {
    if errors.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for error in errors {
        let _ = writeln!(
            out,
            "{} {}: {}",
            "error".if_supports_color(Stdout, |t| t.red()),
            error.path.display(),
            error.message
        );
    }
    let _ = writeln!(
        out,
        "{}",
        format!("{} file(s) could not be formatted and were NOT checked.", errors.len())
            .if_supports_color(Stdout, |t| t.red())
    );
    out
}

/// Shared body of the format renderers.
///
/// `skipped` is the run-level skip set, which is a superset of the per-result
/// [`FormatResult::skipped`] reasons: an explicitly named path that no engine
/// covers never produces a [`FormatResult`] at all, so it can only be counted
/// from here.
fn render_format_core(
    results: &[FormatResult],
    skipped: &[SkippedFile],
    discovery: &DiscoveryReport,
    check: bool,
    verbosity: Verbosity,
) -> (String, usize) {
    let mut out = String::new();
    let changed: Vec<&FormatResult> = results.iter().filter(|r| r.changed).collect();
    for r in &changed {
        let verb = if check { "would reformat" } else { "reformatted" };
        let _ = writeln!(
            out,
            "{} {}",
            verb.if_supports_color(Stdout, |t| t.yellow()),
            r.path.display()
        );
    }
    let scanned = results.len();
    let declined = results.iter().filter(|r| r.skipped.is_some()).count();
    let checked = scanned - declined;
    let n = changed.len();
    if n == 0 {
        // `scanned` counted files that were discovered and routed, including
        // those every backend declined — so a skipped file read exactly like a
        // verified one. Report what was actually inspected, and name the skips.
        let mut tail = format!("{checked} file(s) checked");
        if let Some(clause) = skipped_clause(skipped) {
            let _ = write!(tail, ", {clause}");
        }
        if let Some(clause) = exclusion_clause(discovery) {
            let _ = write!(tail, ", {clause}");
        }
        // A green "All formatted." over an empty file set is the reassuring lie
        // this feature exists to remove: when the exclude set is the reason
        // nothing was checked, say so instead.
        let headline = if checked == 0 && (!discovery.is_empty() || !skipped.is_empty()) {
            "Nothing was checked."
                .if_supports_color(Stdout, |t| t.yellow())
                .to_string()
        } else {
            "All formatted.".if_supports_color(Stdout, |t| t.green()).to_string()
        };
        let _ = writeln!(out, "{headline} ({tail})");
    } else {
        let phrase = if check {
            format!("{n} file(s) will change")
        } else {
            format!("{n} changed")
        };
        let mut tail = format!("of {scanned} file(s)");
        // A partial result is no more trustworthy than a clean one: qualify it
        // with the same skip and exclusion accounting. Reporting drift used to
        // drop the skip clause entirely, so a run that both changed files and
        // declined others said nothing about the second half.
        let mut qualifiers: Vec<String> = Vec::with_capacity(2);
        if let Some(clause) = skipped_clause(skipped) {
            qualifiers.push(clause);
        }
        if let Some(clause) = exclusion_clause(discovery) {
            qualifiers.push(clause);
        }
        if !qualifiers.is_empty() {
            let _ = write!(tail, " ({})", qualifiers.join(", "));
        }
        let _ = writeln!(out, "\n{} {tail}", phrase.if_supports_color(Stdout, |t| t.yellow()));
    }
    push_discovery_note(&mut out, discovery);
    push_skip_note(&mut out, skipped, verbosity.verbose);
    if verbosity.debug {
        for r in results {
            if let Some(debug) = &r.debug {
                let _ = writeln!(out, "{}", r.path.display().if_supports_color(Stdout, |t| t.bold()));
                render_debug_block(&mut out, debug);
            }
        }
    }
    (out, n)
}

/// Print the human-oriented format report to stdout. Returns the number of
/// changed files.
pub fn report_format_pretty(results: &[FormatResult], check: bool, verbosity: Verbosity) -> usize {
    let (text, n) = render_format_pretty(results, check, verbosity);
    print!("{text}");
    n
}

/// Print the human-oriented format report for a whole [`FormatRun`] to stdout,
/// so the summary is qualified by what discovery excluded. Returns the number of
/// changed files.
pub fn report_format_pretty_run(run: &FormatRun, check: bool, verbosity: Verbosity) -> usize {
    let (text, n) = render_format_pretty_run(run, check, verbosity);
    print!("{text}");
    n
}

/// Render format results as pretty-printed JSON.
pub fn report_format_json(results: &[FormatResult]) -> String {
    serde_json::to_string_pretty(results).unwrap_or_else(|_| "[]".to_string())
}

/// Render format results as TOON. Falls back to JSON if TOON serialization
/// fails so output is never silently dropped.
pub fn report_format_toon(results: &[FormatResult]) -> String {
    serde_toon::to_string(&results).unwrap_or_else(|_| report_format_json(results))
}

/// The format results of a run, with one entry appended per skipped path that
/// has no result of its own.
///
/// A file a backend declined already appears in [`FormatRun::results`] carrying
/// its `skipped` reason; a path named on the command line that no engine covers
/// never becomes a result at all, and is what this adds — so the JSON answer to
/// "what did you not look at?" is complete.
fn format_results_with_skips(run: &FormatRun) -> Vec<FormatResult> {
    let mut results = run.results.clone();
    let known: std::collections::BTreeSet<&std::path::Path> = run.results.iter().map(|r| r.path.as_path()).collect();
    results.extend(
        run.skipped
            .iter()
            .filter(|entry| !known.contains(entry.path.as_path()))
            .map(|entry| FormatResult {
                path: entry.path.clone(),
                changed: false,
                skipped: Some(entry.reason.clone()),
                formatted: None,
                debug: None,
            }),
    );
    results
}

/// [`report_format_json`] over a whole [`FormatRun`], so every skipped path is
/// carried structurally.
pub fn report_format_json_run(run: &FormatRun) -> String {
    report_format_json(&format_results_with_skips(run))
}

/// [`report_format_toon`] over a whole [`FormatRun`], including every skipped
/// path.
pub fn report_format_toon_run(run: &FormatRun) -> String {
    report_format_toon(&format_results_with_skips(run))
}
