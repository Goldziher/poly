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
//!    - `@dedent.line_start` → **closer, conditionally**: −1 on the line, but
//!      only when the captured token is the first non-whitespace on it.
//!    - `@auto` / `@indent.auto` / `@ignore` / `@indent.ignore` → **auto**: the
//!      strictly interior lines of the node's range are emitted verbatim.
//!    - `@indent.keep` → **keep**: every line of the node's range after the
//!      first is emitted verbatim, truncated where an inner block opens.
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

thread_local! {
    static QUERY_STATE: RefCell<HashMap<String, (RawParser, QueryCursor)>> =
        RefCell::new(HashMap::new());
}

static BUILTIN_QUERIES: &[(&str, &str)] = &[("elixir", ELIXIR_INDENTS), ("bash", BASH_INDENTS)];

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
/// - `(map)` / `(list)` / `(tuple)` / `(bitstring)` as `@indent.auto`: these are
///   emitted verbatim rather than reindented. `mix format` aligns a wrapped `=>`
///   or operator continuation past the key (`+4` under a `+2` entry), which a
///   level-counting model cannot express — tagging them `@indent` would trade
///   one disagreement with `mix format` for a smaller one, and both oscillate.
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

; Data-literal containers this model cannot indent faithfully — leave interiors
; byte-for-byte so poly and `mix format` converge instead of fighting.
(map) @indent.auto
(list) @indent.auto
(tuple) @indent.auto
(bitstring) @indent.auto
"#;

/// Minimal bash indents query for tier-2 structural reindentation.
///
/// Shell blocks are keyword-delimited (`if`/`fi`, `do`/`done`, `case`/`esac`)
/// rather than brace-delimited, and bash emits *unbalanced* bracket tokens — a
/// lone `)` closes a `case` pattern, while `$(`, `((`, `${` and `[[` are single
/// multi-character tokens. The bracket-counting `BRACE_FAMILY` path would both
/// flatten every keyword block to column 0 and pop a stack nothing pushed, so
/// bash is modelled here instead.
///
/// The model deliberately matches `shfmt`'s default layout, because both can
/// run over the same file (`shfmt` is default-on when installed and supersedes
/// this tier; without it this query is what a shell file gets):
///
/// - `(if_statement)` / `(do_group)` / `(compound_statement)` / `(subshell)` /
///   `(command_substitution)` / `(process_substitution)` / `(array)` as
///   `@indent`: interiors take one level. `while`/`until`/`for`/`select` are
///   covered through their shared `do_group` child, so the loop header itself
///   is never double-counted.
/// - `(case_item)` as `@indent` — and `(case_statement)` deliberately *not*:
///   `shfmt`'s default (switch-case indentation off) leaves the patterns at the
///   `case` level and indents only each item's body and its `;;` terminator.
/// - Closing keywords and brackets as `@dedent.line_start`: `fi`, `done`, `}`,
///   `)`, plus the `then` / `elif` / `else` branch keywords, which sit at the
///   opening construct's level. The line-start condition is load-bearing —
///   `if [ y ]; then echo; fi` written inline inside a function body must keep
///   the body's indent, and an unconditional `-1` on its trailing `fi` would
///   pull the whole line out of the block.
/// - `(command)` / `(list)` / `(pipeline)` / `(test_command)` /
///   `(arithmetic_expansion)` as `@indent.keep`: these are the multi-line
///   *continuation* shapes (`\` line joins, `&&`/`||` lists, `|` pipelines).
///   A level-counting model cannot tell an aligned continuation from an
///   indented one, and bash's `list` nodes nest left-recursively so treating
///   them as openers would indent `a && \n b && \n c` by a growing amount.
///   Every line after the first is emitted verbatim instead, so poly never
///   fights the author's — or `shfmt`'s — chosen continuation alignment.
///
/// Heredocs are not handled here: their bodies *and* terminators are protected
/// wholesale by [`collect_protected_ranges`], because a `<<EOT` terminator that
/// is reindented no longer terminates the heredoc.
const BASH_INDENTS: &str = r#"
; Keyword- and bracket-delimited blocks whose interiors take one level.
(if_statement) @indent
(do_group) @indent
(compound_statement) @indent
(subshell) @indent
(command_substitution) @indent
(process_substitution) @indent
(array) @indent

; `case` patterns stay at the `case` level (shfmt's default); only each item's
; body and its `;;` terminator indent, so the item — not the statement — opens.
(case_item) @indent

; Closing keywords/brackets return their own line to the opening level, but only
; when they start that line: a trailing `fi`/`}` on a one-line statement is
; incidental and must not dedent the statement.
(if_statement "fi" @dedent.line_start)
(if_statement "then" @dedent.line_start)
(elif_clause "elif" @dedent.line_start)
(elif_clause "then" @dedent.line_start)
(else_clause "else" @dedent.line_start)
(do_group "done" @dedent.line_start)
(compound_statement "}" @dedent.line_start)
(subshell ")" @dedent.line_start)
(command_substitution ")" @dedent.line_start)
(process_substitution ")" @dedent.line_start)
(array ")" @dedent.line_start)

; Continuation shapes: every line after the first is emitted byte-for-byte.
(command) @indent.keep
(list) @indent.keep
(pipeline) @indent.keep
(test_command) @indent.keep
(arithmetic_expansion) @indent.keep
"#;

thread_local! {
    static BUILTIN_STATE: RefCell<HashMap<String, (RawParser, QueryCursor, Query)>> =
        RefCell::new(HashMap::new());
}

/// Attempt query-driven reindentation using a poly built-in indents query.
///
/// Called when [`try_reindent_query`] returns `None` (i.e. the language pack
/// does not bundle an `indents.scm` for `name`). Returns `Some(formatted)` when
/// a built-in query exists and parsing succeeds; returns `None` to signal the
/// caller to fall back to whitespace normalization.
pub fn try_reindent_builtin(name: &str, src: &SourceFile, cfg: &EngineConfig) -> Option<String> {
    let query_src = BUILTIN_QUERIES.iter().find(|(n, _)| *n == name).map(|(_, q)| *q)?;

    let (adjustments, protected) = BUILTIN_STATE.with(|cell| {
        let mut pool = cell.borrow_mut();
        if !pool.contains_key(name) {
            let language = get_language(name).ok()?;
            let mut parser = RawParser::new();
            parser.set_language(&language).ok()?;
            let query = Query::new(&language, query_src).ok()?;
            pool.insert(name.to_string(), (parser, QueryCursor::new(), query));
        }
        let entry = pool.get_mut(name)?;
        let tree = entry.0.parse(src.content.as_bytes(), None)?;
        // ~keep A tree containing ERROR/MISSING nodes does not describe the file's real
        // structure, and the damage is not merely cosmetic: a heredoc swallowed by an error
        // node is no longer collected as a protected range, so its body and terminator would
        // be reindented and the script silently broken. Decline the whole file instead.
        if tree.root_node().has_error() {
            return None;
        }
        let adjustments = collect_adjustments(&entry.2, &mut entry.1, &tree, src.content.as_bytes());
        let protected = collect_protected_ranges(&tree, src.content.as_bytes());
        Some((adjustments, protected))
    })?;

    // ~keep A query that captured nothing means poly has no structural model of this file, and
    // `emit_reindented` trims every line and re-emits it at the computed level — which is 0 when
    // there are no openers. Falling through to whitespace normalization keeps an unmodeled file
    // intact instead of flattening it to column 0.
    if adjustments.is_empty() {
        return None;
    }

    Some(emit_reindented(src, cfg, name, &adjustments, &protected))
}

/// Attempt query-driven reindentation for the given grammar and source.
///
/// Returns `Some(formatted_source)` when a bundled indents query exists for
/// `name` and reindentation succeeds. Returns `None` to signal the caller to
/// fall back to brace counting or whitespace normalization.
pub fn try_reindent_query(name: &str, src: &SourceFile, cfg: &EngineConfig) -> Option<String> {
    get_indents_query(name)?;

    let query = get_query(name, QueryKind::Indents).ok()??;

    let (adjustments, protected) = QUERY_STATE.with(|cell| {
        let mut pool = cell.borrow_mut();
        if !pool.contains_key(name) {
            let language = get_language(name).ok()?;
            let mut parser = RawParser::new();
            parser.set_language(&language).ok()?;
            pool.insert(name.to_string(), (parser, QueryCursor::new()));
        }
        let entry = pool.get_mut(name)?;
        let tree = entry.0.parse(src.content.as_bytes(), None)?;
        let adjustments = collect_adjustments(&query, &mut entry.1, &tree, src.content.as_bytes());
        let protected = collect_protected_ranges(&tree, src.content.as_bytes());
        Some((adjustments, protected))
    })?;

    Some(emit_reindented(src, cfg, name, &adjustments, &protected))
}

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
    /// `@dedent.line_start` — the line gets −1 only when the captured token is
    /// the first non-whitespace on its line. A poly-specific capture: a
    /// keyword-delimited grammar closes blocks with words (`fi`, `done`, `}`)
    /// that also appear mid-line in single-line forms (`if x; then y; fi`),
    /// where an unconditional −1 would pull the whole statement out of its
    /// enclosing block.
    CloserIfLineStart,
    /// `@auto` / `@indent.auto` / `@ignore` / `@indent.ignore` — strictly
    /// interior lines of the node range are emitted verbatim (no reindent).
    Auto,
    /// `@indent.keep` — every line of the node's range *after the first* is
    /// emitted verbatim. A poly-specific capture for continuation shapes
    /// (backslash line joins, `&&`/`||` lists, pipelines) whose trailing line
    /// carries meaning [`CaptureKind::Auto`]'s strictly-interior rule would
    /// miss.
    Keep,
    /// All other capture names — no effect on indentation.
    Other,
}

fn classify_capture(name: &str) -> CaptureKind {
    match name {
        "indent" | "indent.begin" | "aligned_indent" | "indent.align" => CaptureKind::Opener,
        "indent_end" | "indent.end" => CaptureKind::CloserAlways,
        "branch" | "indent.branch" | "indent.dedent" | "outdent" => CaptureKind::CloserIfNotOpen,
        "dedent.line_start" => CaptureKind::CloserIfLineStart,
        "auto" | "indent.auto" | "ignore" | "indent.ignore" => CaptureKind::Auto,
        "indent.keep" => CaptureKind::Keep,
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

/// Everything one query run contributes to the indent computation.
struct Adjustments {
    /// `(start_row, end_row)` for each opener node.
    openers: Vec<Opener>,
    /// Deduplicated `(row, start_byte)` pairs for closer tokens.
    closers: CloserList,
    /// `(start_row, end_row)` for regions whose *strictly interior* lines are
    /// emitted verbatim.
    auto_ranges: Vec<(usize, usize)>,
    /// `(start_row, end_row)` for regions whose lines *after the first* are
    /// emitted verbatim. Single-row nodes are dropped at collection time: they
    /// cover no line, and the capture that produces them (`@indent.keep` on a
    /// `command`) fires on nearly every statement in a shell script.
    keep_ranges: Vec<(usize, usize)>,
}

impl Adjustments {
    /// True when the query captured nothing the emitter could act on.
    fn is_empty(&self) -> bool {
        self.openers.is_empty() && self.closers.is_empty() && self.auto_ranges.is_empty() && self.keep_ranges.is_empty()
    }
}

/// Whether `byte` is preceded on its line only by spaces and tabs, i.e. the
/// token starting there is the first non-whitespace on that line.
fn starts_line(source: &[u8], byte: usize) -> bool {
    source[..byte.min(source.len())]
        .iter()
        .rev()
        .take_while(|&&b| b != b'\n')
        .all(|&b| b == b' ' || b == b'\t')
}

/// Walk every query match and sort captures into openers, closers, auto ranges
/// and keep ranges.
fn collect_adjustments(
    query: &tree_sitter::Query,
    cursor: &mut QueryCursor,
    tree: &tree_sitter::Tree,
    source: &[u8],
) -> Adjustments {
    let mut openers = Vec::new();
    let mut closer_bytes: Vec<(usize, usize)> = Vec::new();
    let mut auto_ranges: Vec<(usize, usize)> = Vec::new();
    let mut keep_ranges: Vec<(usize, usize)> = Vec::new();

    let mut matches = cursor.matches(query, tree.root_node(), source);
    while let Some(m) = matches.next() {
        for cap in m.captures() {
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
                    if !is_opening_bracket(node.kind()) {
                        closer_bytes.push((node.start_position().row, node.start_byte()));
                    }
                }
                CaptureKind::CloserIfLineStart => {
                    if starts_line(source, node.start_byte()) {
                        closer_bytes.push((node.start_position().row, node.start_byte()));
                    }
                }
                CaptureKind::Auto => {
                    auto_ranges.push((node.start_position().row, node.end_position().row));
                }
                CaptureKind::Keep => {
                    let (start_row, end_row) = (node.start_position().row, node.end_position().row);
                    if start_row < end_row {
                        keep_ranges.push((start_row, end_row));
                    }
                }
                CaptureKind::Other => {}
            }
        }
    }

    closer_bytes.sort_unstable();
    closer_bytes.dedup();
    let openers = coalesce_openers_by_start_row(openers);
    truncate_keeps_at_inner_openers(&openers, &mut keep_ranges);

    Adjustments {
        openers,
        closers: closer_bytes,
        auto_ranges,
        keep_ranges,
    }
}

/// Collapse openers that begin on the same source line into one indent level —
/// the level-keyed-by-open-line rule the bracket path already documents.
///
/// Two nodes sharing a start row are always nested (one contains the other), so
/// the group keeps the widest end row and the lines they both cover earn a
/// single level. Counting them separately indents a body once per construct
/// that happened to open on the header line — `pattern) [ … ] || {` would put
/// its statements two levels past a pattern sitting at column 0.
fn coalesce_openers_by_start_row(openers: Vec<Opener>) -> Vec<Opener> {
    let mut widest: HashMap<usize, usize> = HashMap::with_capacity(openers.len());
    for opener in &openers {
        widest
            .entry(opener.start_row)
            .and_modify(|end| *end = (*end).max(opener.end_row))
            .or_insert(opener.end_row);
    }
    let mut coalesced: Vec<Opener> = widest
        .into_iter()
        .map(|(start_row, end_row)| Opener { start_row, end_row })
        .collect();
    // A HashMap drain has no defined order; sort so the emitted output cannot
    // depend on it. (The level is a count, so order does not change the result
    // today — this keeps that independent of how the count is taken.)
    coalesced.sort_unstable_by_key(|o| (o.start_row, o.end_row));
    coalesced
}

/// End every keep range at the first opener that begins inside it.
///
/// A continuation shape can span a real block: `cmd | while …; do … done` is one
/// `pipeline` covering the whole loop. Keeping all of it verbatim would leave
/// the loop body at whatever indentation it arrived with while the pipeline's
/// own line reindented, producing a file indented in two different units. The
/// lines *before* the inner block opens are still genuine continuations, so the
/// range is truncated rather than dropped.
fn truncate_keeps_at_inner_openers(openers: &[Opener], keep_ranges: &mut Vec<(usize, usize)>) {
    if keep_ranges.is_empty() || openers.is_empty() {
        return;
    }
    let mut starts: Vec<usize> = openers.iter().map(|o| o.start_row).collect();
    starts.sort_unstable();
    for range in keep_ranges.iter_mut() {
        let index = starts.partition_point(|&row| row < range.0);
        if let Some(&first_inner) = starts.get(index)
            && first_inner <= range.1
        {
            range.1 = first_inner;
        }
    }
    keep_ranges.retain(|&(start, end)| start < end);
}

/// Walk the parsed tree once, collecting `[start_byte, end_byte)` ranges of
/// string-literal, comment, and heredoc nodes. Leading whitespace inside a
/// multi-line string, heredoc, raw string, or block comment is semantically
/// significant, so any line whose start falls inside such a range must be
/// emitted verbatim rather than reindented (mirrors the brace path's
/// protected-range guard).
///
/// Heredocs are collected differently from strings. A string opens mid-line, so
/// its own opening byte is excluded and the line that starts it is still
/// reindented as code; a heredoc's *entire remainder* — body lines and the
/// terminator line — is literal. The terminator matters most: reindenting a
/// `<<EOT` terminator stops it terminating the heredoc and silently breaks the
/// script. So a heredoc contributes the range from the first newline inside it
/// through its last byte, which covers every body line and the terminator line
/// while leaving the `cat <<EOT` line itself free to reindent.
///
/// The walk does not descend into a protected node's subtree — the whole node
/// is treated as one opaque range. Ranges are returned sorted by start byte and
/// with overlaps merged, so [`is_interior`] can binary-search them: without the
/// merge, a range nested inside or straddling another would be shadowed by the
/// `partition_point` lookup and its lines reindented.
fn collect_protected_ranges(tree: &tree_sitter::Tree, source: &[u8]) -> Vec<(usize, usize)> {
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut cursor = tree.root_node().walk();
    'walk: loop {
        let node = cursor.node();
        let kind = node.kind();
        let is_heredoc = kind.contains("heredoc");
        let is_protected = is_heredoc || kind.contains("string") || kind.contains("comment");
        if is_heredoc {
            if let Some(offset) = source[node.start_byte()..node.end_byte()]
                .iter()
                .position(|&b| b == b'\n')
            {
                ranges.push((node.start_byte() + offset, node.end_byte()));
            }
        } else if is_protected {
            ranges.push((node.start_byte(), node.end_byte()));
        }
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
    merge_overlapping(ranges)
}

/// Collapse strictly overlapping `[start, end)` ranges, preserving the input
/// order (which must already be sorted by start byte). Ranges that merely abut
/// (`next.start == previous.end`) are left separate: merging them would pull
/// the second range's opening byte inside the first, and that byte is
/// deliberately excluded by [`is_interior`].
fn merge_overlapping(ranges: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
    for (start, end) in ranges {
        match merged.last_mut() {
            Some(last) if start < last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    merged
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
    adjustments: &Adjustments,
    protected: &[(usize, usize)],
) -> String {
    let Adjustments {
        openers,
        closers,
        auto_ranges,
        keep_ranges,
    } = adjustments;
    let unit = super::indent_unit(grammar, cfg.indent_width);
    let line_ending = cfg.globals.line_ending.as_str();

    let mut closer_by_row: HashMap<usize, usize> = HashMap::new();
    for &(row, _) in closers {
        *closer_by_row.entry(row).or_insert(0) += 1;
    }

    const MAX_DEPTH: usize = 64;
    let max_indent = unit.repeat(MAX_DEPTH);

    let mut out = String::with_capacity(src.content.len() + src.content.len() / 8);
    let mut first = true;
    let mut byte = 0usize;

    for (line_idx, raw) in src.content.split('\n').enumerate() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let line_start = byte;
        byte += raw.len() + 1;

        if !first {
            out.push_str(line_ending);
        }
        first = false;

        if is_interior(protected, line_start) {
            out.push_str(line);
            continue;
        }

        if keep_ranges.iter().any(|&(s, e)| s < line_idx && line_idx <= e) {
            out.push_str(line);
            continue;
        }

        if auto_ranges.iter().any(|&(s, e)| s < line_idx && line_idx < e) {
            out.push_str(line);
            continue;
        }

        let opener_count: usize = openers
            .iter()
            .filter(|o| o.start_row < line_idx && line_idx <= o.end_row)
            .count();

        let close_count: usize = closer_by_row.get(&line_idx).copied().unwrap_or(0);

        let level = opener_count.saturating_sub(close_count);

        let trimmed = line.trim();
        if !trimmed.is_empty() {
            let indent_bytes = level * unit.len();
            if indent_bytes <= max_indent.len() {
                out.push_str(&max_indent[..indent_bytes]);
            } else {
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
