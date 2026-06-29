//! Query-driven reindentation for the tier-2 generic formatter.
//!
//! When a language has a bundled `indents.scm` in `tree-sitter-language-pack`,
//! this module drives reindentation by running the compiled query against the
//! parse tree rather than doing raw brace counting.
//!
//! ## Algorithm
//!
//! 1. Fetch the compiled, process-cached query via
//!    [`get_query`]`(name, QueryKind::Indents)`.
//! 2. Parse the source with a thread-local raw `tree_sitter::Parser` pool.
//! 3. Walk every query match and classify each capture:
//!    - `@indent` / `@indent.begin` / `@aligned_indent` → **opener**: every line
//!      strictly after the node's start row and up to (inclusive) its end row
//!      receives +1 indent.
//!    - `@indent_end` / `@indent.end` / `@branch` / `@indent.branch` /
//!      `@indent.dedent` / `@outdent` → **closer**: the line that contains this
//!      token receives −1 indent (multiple captures on the same byte deduplicate
//!      to one −1).
//!    - `@auto` / `@indent.auto` / `@ignore` / `@indent.ignore` → **auto**: the
//!      strictly interior lines of the node's range are emitted verbatim.
//!    - All other capture names are ignored.
//! 4. For each line `L`:  `level = max(0, openers_covering_L − closers_on_L)`.
//! 5. Re-emit each non-empty line as `indent_unit.repeat(level) + trimmed`.
//!
//! The function returns `None` (triggering fallback) when no indents query is
//! bundled for the grammar, when the grammar cannot be loaded, or when the
//! source fails to parse.

use std::cell::RefCell;
use std::collections::HashMap;

use tree_sitter::{Parser as RawParser, QueryCursor, StreamingIterator};
use tree_sitter_language_pack::{QueryKind, get_indents_query, get_language, get_query};

use crate::config::EngineConfig;
use crate::engine::SourceFile;

// Per-thread pool of raw `tree_sitter::Parser`s + `QueryCursor`s, keyed by
// grammar name. Separate from the tslp-wrapped `PARSERS` pool in `mod.rs`
// because `tree_sitter::Query::matches` requires a raw `tree_sitter::Node`
// (only obtainable from a `tree_sitter::Tree`, not from tslp's owned wrapper).
// The `QueryCursor` is pooled alongside the parser so the warm path allocates
// neither per file.
thread_local! {
    static QUERY_STATE: RefCell<HashMap<String, (RawParser, QueryCursor)>> =
        RefCell::new(HashMap::new());
}

/// Attempt query-driven reindentation for the given grammar and source.
///
/// Returns `Some(formatted_source)` when a bundled indents query exists for
/// `name` and reindentation succeeds. Returns `None` to signal the caller to
/// fall back to brace counting or whitespace normalization.
pub fn try_reindent_query(name: &str, src: &SourceFile, cfg: &EngineConfig) -> Option<String> {
    // Fast-path: skip if no indents query is bundled for this grammar.
    // `get_indents_query` reads a static table — no I/O.
    get_indents_query(name)?;

    // Fetch the compiled, process-cached query (compiled once per process lifetime).
    let query = get_query(name, QueryKind::Indents).ok()??;

    // Parse + collect adjustments using the thread-local parser/cursor pool.
    // Both the parser and the QueryCursor are reused across files on this
    // worker thread, so the warm path performs no per-file allocation here.
    let (openers, closers, auto_ranges) = QUERY_STATE.with(|cell| {
        let mut pool = cell.borrow_mut();
        if !pool.contains_key(name) {
            let language = get_language(name).ok()?;
            let mut parser = RawParser::new();
            parser.set_language(&language).ok()?;
            pool.insert(name.to_string(), (parser, QueryCursor::new()));
        }
        // Guaranteed present by the insert above.
        let entry = pool.get_mut(name)?;
        let tree = entry.0.parse(src.content.as_bytes(), None)?;
        Some(collect_adjustments(
            &query,
            &mut entry.1,
            &tree,
            src.content.as_bytes(),
        ))
    })?;

    // Re-emit each line at its computed indent level.
    Some(emit_reindented(
        src,
        cfg,
        name,
        &openers,
        &closers,
        &auto_ranges,
    ))
}

// ── Internal helpers ─────────────────────────────────────────────────────────

/// Classification of a query capture name for indentation purposes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CaptureKind {
    /// `@indent` / `@indent.begin` / `@aligned_indent` — container node whose
    /// interior lines (strictly after start row, up to end row inclusive) get +1.
    Opener,
    /// `@indent_end` / `@indent.end` — the line containing this token gets −1
    /// unconditionally (by convention only placed on closing delimiters).
    CloserAlways,
    /// `@branch` / `@indent.branch` / `@indent.dedent` / `@outdent` — the line
    /// gets −1 UNLESS the token is an opening bracket (`{`/`(`/`[`/`<`).
    /// Many grammars tag both `{` and `}` as `@branch`; the −1 only makes sense
    /// for the *closing* half so that `values: [` stays at its correct depth.
    CloserIfNotOpen,
    /// `@auto` / `@indent.auto` / `@ignore` / `@indent.ignore` — strictly
    /// interior lines of the node range are emitted verbatim (no reindent).
    Auto,
    /// All other capture names — no effect on indentation.
    Other,
}

fn classify_capture(name: &str) -> CaptureKind {
    match name {
        "indent" | "indent.begin" | "aligned_indent" | "indent.align" => CaptureKind::Opener,
        "indent_end" | "indent.end" => CaptureKind::CloserAlways,
        "branch" | "indent.branch" | "indent.dedent" | "outdent" => CaptureKind::CloserIfNotOpen,
        "auto" | "indent.auto" | "ignore" | "indent.ignore" => CaptureKind::Auto,
        _ => CaptureKind::Other,
    }
}

/// True when `kind` is a bracket-open token that should never trigger a −1.
/// Many grammars tag `[ "{" "}" ] @branch`; the opening half must be skipped.
fn is_opening_bracket(kind: &str) -> bool {
    matches!(kind, "{" | "(" | "[" | "<")
}

/// A node whose interior lines should be indented by one level.
struct Opener {
    start_row: usize,
    end_row: usize,
}

/// Closer tokens per row (row, start_byte) — deduplicated.
type CloserList = Vec<(usize, usize)>;

/// Walk every query match and sort captures into openers, closers, and auto ranges.
///
/// Returns:
/// - `openers`: `(start_row, end_row)` for each opener node.
/// - `closers`: deduplicated `(row, start_byte)` pairs for closer tokens.
/// - `auto_ranges`: `(start_row, end_row)` for verbatim regions.
fn collect_adjustments(
    query: &tree_sitter::Query,
    cursor: &mut QueryCursor,
    tree: &tree_sitter::Tree,
    source: &[u8],
) -> (Vec<Opener>, CloserList, Vec<(usize, usize)>) {
    let mut openers = Vec::new();
    // (row, start_byte) — sorted + deduped later to count each closer token once.
    let mut closer_bytes: Vec<(usize, usize)> = Vec::new();
    let mut auto_ranges: Vec<(usize, usize)> = Vec::new();

    let mut matches = cursor.matches(query, tree.root_node(), source);
    while let Some(m) = matches.next() {
        for cap in m.captures {
            let cap_name = query.capture_names()[cap.index as usize];
            let node = cap.node;
            match classify_capture(cap_name) {
                CaptureKind::Opener => {
                    openers.push(Opener {
                        start_row: node.start_position().row,
                        end_row: node.end_position().row,
                    });
                }
                CaptureKind::CloserAlways => {
                    closer_bytes.push((node.start_position().row, node.start_byte()));
                }
                CaptureKind::CloserIfNotOpen => {
                    // Skip opening brackets tagged with @branch — many grammars
                    // tag `[ "{" "}" ] @branch` together, but the −1 is only
                    // meaningful for closing brackets and non-bracket keywords.
                    if !is_opening_bracket(node.kind()) {
                        closer_bytes.push((node.start_position().row, node.start_byte()));
                    }
                }
                CaptureKind::Auto => {
                    auto_ranges.push((node.start_position().row, node.end_position().row));
                }
                CaptureKind::Other => {}
            }
        }
    }

    // Deduplicate: multiple captures on the same token (e.g. `@indent_end @branch`
    // on the same `}`) should count as a single −1, not two.
    closer_bytes.sort_unstable();
    closer_bytes.dedup();

    (openers, closer_bytes, auto_ranges)
}

/// Re-emit `src.content` with each line at its computed indent level.
fn emit_reindented(
    src: &SourceFile,
    cfg: &EngineConfig,
    grammar: &str,
    openers: &[Opener],
    closers: &CloserList,
    auto_ranges: &[(usize, usize)],
) -> String {
    let unit = super::indent_unit(grammar, cfg.indent_width);
    let line_ending = cfg.globals.line_ending.as_str();

    // Precompute per-line closer counts (deduplication already applied).
    let mut closer_by_row: HashMap<usize, usize> = HashMap::new();
    for &(row, _) in closers {
        *closer_by_row.entry(row).or_insert(0) += 1;
    }

    // Precompute a 64-level indent string: slice into it for a single push_str
    // per line instead of `level` separate pushes.
    const MAX_DEPTH: usize = 64;
    let max_indent = unit.repeat(MAX_DEPTH);

    let mut out = String::with_capacity(src.content.len() + src.content.len() / 8);
    let mut first = true;

    for (line_idx, raw) in src.content.split('\n').enumerate() {
        // Strip '\r' for CRLF; re-added via `line_ending`.
        let line = raw.strip_suffix('\r').unwrap_or(raw);

        if !first {
            out.push_str(line_ending);
        }
        first = false;

        // Strictly interior lines of @auto/@ignore nodes: emit verbatim.
        // The first line of the node (start_row) is normal code; only lines
        // between start and end (exclusive of both boundaries) are protected.
        if auto_ranges
            .iter()
            .any(|&(s, e)| s < line_idx && line_idx < e)
        {
            out.push_str(line);
            continue;
        }

        // Opener count: how many @indent nodes span this line
        // (i.e. start before this line, end at or after this line).
        let opener_count: usize = openers
            .iter()
            .filter(|o| o.start_row < line_idx && line_idx <= o.end_row)
            .count();

        // Closer count: how many unique closer tokens are on this line.
        let close_count: usize = closer_by_row.get(&line_idx).copied().unwrap_or(0);

        let level = opener_count.saturating_sub(close_count);

        let trimmed = line.trim();
        if !trimmed.is_empty() {
            let indent_bytes = level * unit.len();
            if indent_bytes <= max_indent.len() {
                out.push_str(&max_indent[..indent_bytes]);
            } else {
                // Pathological depth: fall back to the per-level loop.
                for _ in 0..level {
                    out.push_str(&unit);
                }
            }
            out.push_str(trimmed);
        }
    }

    super::apply_trailing_newline(
        &mut out,
        &src.content,
        line_ending,
        cfg.globals.final_newline,
    );
    out
}
