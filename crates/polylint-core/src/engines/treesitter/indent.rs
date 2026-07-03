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

use tree_sitter::{Parser as RawParser, Query, QueryCursor, StreamingIterator};
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

// ── Polylint built-in indents queries ────────────────────────────────────────
//
// Some grammars ship without a bundled `indents.scm` in tree-sitter-language-pack
// (e.g. Elixir's grammar is maintained by the Elixir core team and has not yet
// contributed an indents query). Polylint provides minimal hand-written queries
// here so those languages still receive structural reindentation via the same
// query-driven path that bundled languages use.
//
// Each entry is `(grammar_name, query_source)`.
static BUILTIN_QUERIES: &[(&str, &str)] = &[("elixir", ELIXIR_INDENTS)];

/// Minimal Elixir indents query for tier-2 structural reindentation.
///
/// Elixir uses `do...end` blocks where braces never appear as block delimiters,
/// so the brace-counting BRACE_FAMILY path cannot reindent it. This query
/// captures the key structural nodes:
///
/// - `(do_block)` as `@indent`: every `do...end` block (defmodule, def, if,
///   case, for, with, try, receive, …) indents its interior by one level.
/// - `"end"` inside `do_block` as `@indent.end`: the closing keyword brings
///   its own line back to the pre-block depth (−1).
/// - `rescue`/`else`/`catch`/`after` keywords as `@branch`: these sub-block
///   opener keywords appear at the same depth as the surrounding `do`, so the
///   line they appear on gets −1, cancelling the +1 contributed by the
///   enclosing `do_block`.
/// - `(anonymous_function)` / `"end"`: `fn ... end` anonymous functions follow
///   the same indent model as `do_block`.
const ELIXIR_INDENTS: &str = r#"
; do...end blocks (defmodule/def/if/case/for/with/try/receive/…)
(do_block) @indent
(do_block "end" @indent.end)

; rescue/else/catch/after keywords sit at the same depth as the opening `do`,
; so tag them as @branch to apply -1 on the line they appear on.
(rescue_block "rescue" @branch)
(else_block "else" @branch)
(catch_block "catch" @branch)
(after_block "after" @branch)

; fn ... end anonymous functions
(anonymous_function) @indent
(anonymous_function "end" @indent.end)
"#;

// Per-thread pool for polylint built-in queries: (parser, cursor, compiled query)
// keyed by grammar name. The compiled `Query` is stored alongside the parser and
// cursor so the grammar is only loaded and the query is only compiled once per
// thread per grammar, not once per file.
thread_local! {
    static BUILTIN_STATE: RefCell<HashMap<String, (RawParser, QueryCursor, Query)>> =
        RefCell::new(HashMap::new());
}

/// Attempt query-driven reindentation using a polylint built-in indents query.
///
/// Called when [`try_reindent_query`] returns `None` (i.e. the language pack
/// does not bundle an `indents.scm` for `name`). Returns `Some(formatted)` when
/// a built-in query exists and parsing succeeds; returns `None` to signal the
/// caller to fall back to whitespace normalization.
pub fn try_reindent_builtin(name: &str, src: &SourceFile, cfg: &EngineConfig) -> Option<String> {
    // Fast-path: no built-in query for this grammar.
    let query_src = BUILTIN_QUERIES.iter().find(|(n, _)| *n == name).map(|(_, q)| *q)?;

    let (openers, closers, auto_ranges, protected) = BUILTIN_STATE.with(|cell| {
        let mut pool = cell.borrow_mut();
        if !pool.contains_key(name) {
            let language = get_language(name).ok()?;
            let mut parser = RawParser::new();
            parser.set_language(&language).ok()?;
            let query = Query::new(&language, query_src).ok()?;
            pool.insert(name.to_string(), (parser, QueryCursor::new(), query));
        }
        // Guaranteed present by the insert above.
        let entry = pool.get_mut(name)?;
        // Parse first (borrows entry.0 temporarily, returns an owned Tree).
        let tree = entry.0.parse(src.content.as_bytes(), None)?;
        // Then borrow entry.2 (query, immutable) and entry.1 (cursor, mutable)
        // simultaneously — distinct fields, so the borrow checker accepts this.
        let (openers, closers, auto_ranges) =
            collect_adjustments(&entry.2, &mut entry.1, &tree, src.content.as_bytes());
        let protected = collect_protected_ranges(&tree);
        Some((openers, closers, auto_ranges, protected))
    })?;

    Some(emit_reindented(
        src,
        cfg,
        name,
        &openers,
        &closers,
        &auto_ranges,
        &protected,
    ))
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
    let (openers, closers, auto_ranges, protected) = QUERY_STATE.with(|cell| {
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
        let (openers, closers, auto_ranges) = collect_adjustments(&query, &mut entry.1, &tree, src.content.as_bytes());
        // Second pass over the same parsed tree: string/comment byte ranges whose
        // interiors must be emitted verbatim.
        let protected = collect_protected_ranges(&tree);
        Some((openers, closers, auto_ranges, protected))
    })?;

    // Re-emit each line at its computed indent level.
    Some(emit_reindented(
        src,
        cfg,
        name,
        &openers,
        &closers,
        &auto_ranges,
        &protected,
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

/// Walk the parsed tree once, collecting `[start_byte, end_byte)` ranges of
/// string-literal and comment nodes. Leading whitespace inside a multi-line
/// string, heredoc, raw string, or block comment is semantically significant,
/// so any line whose start falls inside such a range must be emitted verbatim
/// rather than reindented (mirrors the brace path's protected-range guard).
///
/// The walk does not descend into a protected node's subtree — the whole node
/// is treated as one opaque range. Ranges are returned sorted by start byte so
/// [`is_interior`] can binary-search them.
fn collect_protected_ranges(tree: &tree_sitter::Tree) -> Vec<(usize, usize)> {
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut cursor = tree.root_node().walk();
    'walk: loop {
        let node = cursor.node();
        let kind = node.kind();
        let is_protected = kind.contains("string") || kind.contains("comment");
        if is_protected {
            ranges.push((node.start_byte(), node.end_byte()));
        }
        // Descend only into non-protected nodes; a string/comment subtree is an
        // opaque protected range.
        if !is_protected && cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                break 'walk;
            }
        }
    }
    ranges.sort_unstable_by_key(|r| r.0);
    ranges
}

/// Whether `byte` falls strictly inside any protected range (`start < byte <
/// end`), i.e. it is an interior byte of a string or comment. The opening byte
/// of a range is excluded so the line that opens the node is still reindented as
/// real code. `protected` must be sorted by start byte (guaranteed by
/// [`collect_protected_ranges`]), so this is O(log n) via `partition_point`.
fn is_interior(protected: &[(usize, usize)], byte: usize) -> bool {
    let pos = protected.partition_point(|&(start, _)| start < byte);
    if pos == 0 {
        return false;
    }
    byte < protected[pos - 1].1
}

/// Re-emit `src.content` with each line at its computed indent level.
fn emit_reindented(
    src: &SourceFile,
    cfg: &EngineConfig,
    grammar: &str,
    openers: &[Opener],
    closers: &CloserList,
    auto_ranges: &[(usize, usize)],
    protected: &[(usize, usize)],
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
    // Byte offset of the current line's start, tracked so protected string /
    // comment interiors can be detected by byte range.
    let mut byte = 0usize;

    for (line_idx, raw) in src.content.split('\n').enumerate() {
        // Strip '\r' for CRLF; re-added via `line_ending`.
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let line_start = byte;
        byte += raw.len() + 1; // raw includes '\r' for CRLF — must not use line.len()

        if !first {
            out.push_str(line_ending);
        }
        first = false;

        // Interior of a multi-line string / comment: leading whitespace is part
        // of the literal, so emit the line verbatim rather than reindenting.
        if is_interior(protected, line_start) {
            out.push_str(line);
            continue;
        }

        // Strictly interior lines of @auto/@ignore nodes: emit verbatim.
        // The first line of the node (start_row) is normal code; only lines
        // between start and end (exclusive of both boundaries) are protected.
        if auto_ranges.iter().any(|&(s, e)| s < line_idx && line_idx < e) {
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

    super::apply_trailing_newline(&mut out, &src.content, line_ending, cfg.globals.final_newline);
    out
}
