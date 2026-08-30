//! Typography for the human-oriented reports: grouped numbers, real plurals,
//! and width-aware sample lines.
//!
//! Three defects this module exists to remove, all of them found by reading
//! poly's own output over a large repository:
//!
//! - `41576 issue(s) found` — an ungrouped five-digit number is read digit by
//!   digit, and `(s)` makes the reader do the grammar. Both cost attention that
//!   belongs on the finding.
//! - A sample list joined with commas ran to 300 characters, so an 80-column
//!   terminal hard-wrapped it mid-path into an unreadable block. Fitting the
//!   samples to the terminal keeps every line whole.
//! - A hint repeated once per skip reason (`— pass --verbose to list them`) was
//!   printed twelve times in one run. It is said once, at the end of the block.

use std::io::IsTerminal;
use std::sync::OnceLock;

/// Width assumed when stdout is not a terminal (a pipe, a file, a test).
///
/// Matches poly's own `[defaults] line_length`, and — being a constant — keeps
/// piped output byte-identical whatever terminal launched the run. A renderer
/// whose output changed with the launching window would make every snapshot
/// test and every CI diff depend on it.
const PIPED_WIDTH: usize = 120;

/// Narrowest width the sample fitter will honour, so a pathologically small
/// terminal still gets one sample per line rather than none.
const MIN_WIDTH: usize = 40;

/// The width the notes lay themselves out for: the terminal's, or
/// [`PIPED_WIDTH`] when stdout is not a terminal.
///
/// Resolved once — a `SIGWINCH` mid-run would otherwise let one report render
/// at two widths.
pub fn width() -> usize {
    static WIDTH: OnceLock<usize> = OnceLock::new();
    *WIDTH.get_or_init(|| {
        if !std::io::stdout().is_terminal() {
            return PIPED_WIDTH;
        }
        terminal_size::terminal_size()
            .map(|(terminal_size::Width(w), _)| usize::from(w))
            .unwrap_or(PIPED_WIDTH)
            .max(MIN_WIDTH)
    })
}

/// A number with thousands separators: `41576` → `41,576`.
pub fn number(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    let first = digits.len() % 3;
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && index % 3 == first {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// A grouped count and the noun that agrees with it: `1 file`, `21,657 files`.
///
/// The `(s)` form poly used to print is a cop-out that reads as machine output;
/// the number is already there, so the noun may as well be right.
pub fn quantity(value: usize, singular: &str, plural: &str) -> String {
    format!("{} {}", number(value), if value == 1 { singular } else { plural })
}

/// `1 file` / `2 files`.
pub fn files(value: usize) -> String {
    quantity(value, "file", "files")
}

/// `1 directory` / `2 directories`.
pub fn directories(value: usize) -> String {
    quantity(value, "directory", "directories")
}

/// `1 issue` / `2 issues`.
pub fn issues(value: usize) -> String {
    quantity(value, "issue", "issues")
}

/// `1 path` / `2 paths`.
pub fn paths(value: usize) -> String {
    quantity(value, "path", "paths")
}

/// `1 rule` / `2 rules`.
pub fn rules(value: usize) -> String {
    quantity(value, "rule", "rules")
}

/// As many of `samples` as fit on one line of the current terminal after
/// `prefix`, joined with `", "`.
///
/// Always returns at least the first sample: a path longer than the terminal is
/// still more useful than no path at all, and it is one wrapped item rather than
/// a wrapped block. Returns `None` for an empty slice.
pub fn fit_samples(prefix: &str, samples: &[String]) -> Option<String> {
    let (first, rest) = samples.split_first()?;
    let budget = width();
    let mut line = format!("{prefix}{first}");
    for sample in rest {
        if line.chars().count() + 2 + sample.chars().count() > budget {
            break;
        }
        line.push_str(", ");
        line.push_str(sample);
    }
    Some(line)
}

/// `items` joined with `", "`, wrapped to the terminal width with every line
/// after the first indented by `hanging`.
///
/// Breaks only *between* items, so a long item wraps once on its own line
/// instead of shredding the block. `first_prefix` is written before the first
/// item and counts against that line's budget.
///
/// Continuation lines align under the first item where the prefix is short
/// enough to leave a usable column (a third of the terminal), and fall back to
/// `hanging` when it is not — an 80-column terminal must not lose half its width
/// to an indent.
pub fn wrap_items(first_prefix: &str, items: &[String], hanging: usize) -> String {
    let budget = width();
    let prefix_len = first_prefix.chars().count();
    let hanging = if prefix_len <= budget / 3 { prefix_len } else { hanging };
    let indent = " ".repeat(hanging);
    let mut out = String::new();
    let mut line = first_prefix.to_owned();
    let mut line_len = line.chars().count();
    let mut first_on_line = true;
    for item in items {
        let item_len = item.chars().count();
        if !first_on_line && line_len + 2 + item_len > budget {
            out.push_str(&line);
            out.push('\n');
            line = indent.clone();
            line_len = hanging;
            first_on_line = true;
        }
        if !first_on_line {
            line.push_str(", ");
            line_len += 2;
        }
        line.push_str(item);
        line_len += item_len;
        first_on_line = false;
    }
    out.push_str(&line);
    out
}
