//! Tier-2 generic formatter: the catch-all backend for every language without
//! a native crate backend. Built on `tree-sitter-language-pack`, which fetches
//! and dynamically loads grammars on demand, so one binary covers the long tail
//! of languages with zero preinstalled language tools.
//!
//! Three modes, chosen per grammar:
//! - **Leave untouched** for data, template, and asset grammars in
//!   `LEAVE_UNTOUCHED`: both `lint` and `format` are no-ops. Whitespace
//!   inside these files is semantically significant output (CSV fields, ERB
//!   template whitespace, diff context lines) — normalizing it silently
//!   corrupts data.
//! - **Structural reindent** for brace-delimited grammars in `BRACE_FAMILY`
//!   (Go, C, C++, Java, Kotlin, Rust, …): the CST locates all bracket tokens
//!   and re-indents each line by depth using a **conservative
//!   level-keyed-by-open-line** model. Multiple brackets opened on the same
//!   line coalesce to one indent level. A level is released when the first
//!   leading closer pops any bracket opened on that line. Byte ranges covered
//!   by string-literal and comment CST nodes are excluded: delimiters inside
//!   them never count toward depth, and any line whose leading whitespace
//!   begins inside such a range is emitted verbatim. The indent unit is
//!   per-grammar (tabs for Go, two spaces for Swift/Dart, `indent_width`
//!   spaces otherwise). Per-language switch/case adjustments apply for Swift
//!   (case labels align with `switch`), Dart, and C# (case bodies get an
//!   extra indent level).
//! - **Whitespace normalization** for every other grammar, and whenever the
//!   grammar is unavailable or the source fails to parse. This never corrupts
//!   unparsable input (it only trims trailing whitespace and fixes line
//!   endings / the final newline).

mod indent;

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;

use tree_sitter_language_pack::{Node, Parser, detect_language, get_parser};

use crate::config::EngineConfig;
use crate::defaults::normalize_whitespace;
use crate::engine::{Capabilities, Engine, FormatOutput, SourceFile};
use crate::language::Language;

thread_local! {
    /// Per-thread parser pool keyed by grammar name. tree-sitter parsers are
    /// expensive to build, so each rayon worker reuses one parser per language
    /// across files instead of constructing one per file.
    static PARSERS: RefCell<HashMap<String, Parser>> = RefCell::new(HashMap::new());
}

/// Generic tree-sitter backend (see module docs).
pub struct TreeSitterEngine;

/// The generic tier declares no tier-1 languages; the registry routes to it as
/// the fallthrough for any language no native backend has claimed.
static LANGUAGES: &[Language] = &[];

/// Grammar names (as known to the language pack) for which bracket-depth
/// structural reindentation is safe: brace-delimited, non-whitespace-sensitive
/// languages. Everything else falls back to whitespace normalization, so a
/// layout-significant language (YAML, Python-likes) is never reflowed.
const BRACE_FAMILY: &[&str] = &[
    "go", "c", "cpp", "java", "kotlin", "rust", "scala", "swift", "php", "csharp", "objc", "proto", "dart", "glsl",
    "hlsl", "cuda", "zig",
];

/// Grammar names for which **both `lint` and `format` are unconditional
/// no-ops**. These are data, template, and asset languages where any whitespace
/// change — even stripping trailing spaces — silently mutates the file's
/// semantic content or rendered output:
///
/// - `csv` / `tsv`: field values; a trailing space inside a field is data.
/// - `embeddedtemplate` (ERB): whitespace around `<% %>` tags is rendered
///   verbatim into the template output.
/// - `jinja2` (.j2/.jinja2): same reasoning as ERB.
/// - `ini` / `properties`: key-value config; value whitespace can be
///   intentional and is consumed literally by many parsers.
/// - `po` (gettext): `msgid`/`msgstr` field content is exact; whitespace
///   changes break translation strings.
/// - `diff` / `patch`: the `+`/`-`/` ` line prefix IS the file structure;
///   stripping trailing whitespace from a context line corrupts the patch.
///
/// Tier-1 backends already own json/yaml/xml/html/svg/toml/graphql/jinja/
/// mustache/vue/svelte, so those never reach this tier.
const LEAVE_UNTOUCHED: &[&str] = &[
    "csv",
    "tsv",
    "embeddedtemplate",
    "jinja2",
    "ini",
    "properties",
    "po",
    "diff",
];

impl Engine for TreeSitterEngine {
    fn name(&self) -> &'static str {
        "treesitter"
    }

    fn languages(&self) -> &'static [Language] {
        LANGUAGES
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            lint: false,
            format: true,
            fix: true,
        }
    }

    fn version(&self) -> &str {
        "8+tslp1.13.2"
    }

    fn format(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
        let name = grammar_name(src);
        if name.as_deref().is_some_and(|n| LEAVE_UNTOUCHED.contains(&n)) {
            return Ok(FormatOutput::Unchanged);
        }
        let formatted = match name {
            Some(name) => {
                if BRACE_FAMILY.contains(&name.as_str()) {
                    reindent_braces(&name, src, cfg).unwrap_or_else(|| normalize_whitespace(&src.content, &cfg.globals))
                } else {
                    indent::try_reindent_query(&name, src, cfg)
                        .or_else(|| indent::try_reindent_builtin(&name, src, cfg))
                        .unwrap_or_else(|| normalize_whitespace(&src.content, &cfg.globals))
                }
            }
            None => normalize_whitespace(&src.content, &cfg.globals),
        };
        if formatted == *src.content {
            Ok(FormatOutput::Unchanged)
        } else {
            Ok(FormatOutput::Formatted(formatted))
        }
    }
}

/// Resolve the language-pack grammar name for a source file. Prefers the pack's
/// own path-based detection (so `.sh` → `bash`), then the explicit
/// [`Language::Other`] id, then the tier-1 [`Language::id`].
fn grammar_name(src: &SourceFile) -> Option<String> {
    let path = src.path.to_string_lossy();
    if let Some(name) = detect_language(&path) {
        return Some(name.to_string());
    }
    if let Language::Other(name) = &src.language {
        return Some(name.clone());
    }
    Some(src.language.id().to_string())
}

/// Parse `src` with the pooled parser for `name` and re-indent by bracket
/// depth. Returns `None` (so the caller falls back to whitespace normalization)
/// when the grammar cannot be loaded or the source cannot be parsed.
fn reindent_braces(name: &str, src: &SourceFile, cfg: &EngineConfig) -> Option<String> {
    PARSERS.with(|cell| {
        let mut pool = cell.borrow_mut();
        if !pool.contains_key(name) {
            let parser = get_parser(name).ok()?;
            pool.insert(name.to_string(), parser);
        }
        let parser = pool.get_mut(name)?;
        let tree = parser.parse(&src.content)?;
        let cst = collect_cst(&tree.root_node(), name);
        Some(reindent(&src.content, name, &cst, cfg))
    })
}

/// A delimiter token located in the CST: its byte offset and whether it opens
/// (`(`, `[`, `{`) or closes (`)`, `]`, `}`). Only tokens outside protected
/// string/comment ranges are collected; interior brackets never count toward
/// depth.
struct Delimiter {
    byte: usize,
    open: bool,
}

/// Structural facts extracted from one CST walk: the bracket delimiters that
/// drive depth, byte ranges of string/comment nodes whose interiors must never
/// be reindented, and per-language switch/case adjustment ranges.
struct CstFacts {
    /// All structural delimiters in source order: openers (`{`, `(`, `[`)
    /// and closers (`}`, `)`, `]`). The depth model is
    /// level-keyed-by-open-line: multiple brackets opened on the same source
    /// line coalesce to one indent level; a level is released by the first
    /// leading closer that pops any bracket opened on that line.
    delimiters: Vec<Delimiter>,
    /// `[start, end)` byte ranges covered by string-literal / comment nodes,
    /// in source order. Used to leave string and comment interiors verbatim.
    protected: Vec<(usize, usize)>,
    /// Lines whose `line_start` falls in `[start, end)` get their computed
    /// indent level reduced by 1. Used for Swift `switch_entry` case-label
    /// lines: swift-format aligns case labels with the `switch` keyword rather
    /// than indenting them into the switch body.
    case_label_dedent: Vec<(usize, usize)>,
    /// Lines whose `line_start` falls in `[start, end)` get their computed
    /// indent level increased by 1. Used for Dart/C# switch case bodies
    /// (implicit extra indent after `case …:`) and, combined with
    /// `case_label_dedent`, for the statement body inside a Swift
    /// `switch_entry` (net effect: body stays at bracket depth while the
    /// label is pulled out one level).
    case_body_extra: Vec<(usize, usize)>,
}

impl CstFacts {
    /// Whether `byte` falls strictly inside any protected range, i.e. it is an
    /// interior byte of a string or comment (`start < byte < end`). The opening
    /// byte of the range is excluded so the line that opens the node is still
    /// reindented as real code.
    ///
    /// `protected` is sorted by start byte (guaranteed by `collect_cst`), so
    /// this is O(log n) via `partition_point` instead of a linear scan.
    fn is_interior(&self, byte: usize) -> bool {
        let pos = self.protected.partition_point(|&(start, _)| start < byte);
        if pos == 0 {
            return false;
        }
        let (_, end) = self.protected[pos - 1];
        byte < end
    }

    /// Net indent-level adjustment for a line whose first byte is `line_start`.
    /// Returns −1, 0, or +1 based on the per-language case/label ranges.
    fn case_adjustment(&self, line_start: usize) -> i32 {
        let dedent = self
            .case_label_dedent
            .iter()
            .any(|&(s, e)| s <= line_start && line_start < e);
        let extra = self
            .case_body_extra
            .iter()
            .any(|&(s, e)| s <= line_start && line_start < e);
        (if extra { 1 } else { 0 }) - (if dedent { 1 } else { 0 })
    }
}

/// Walk the CST once, collecting all structural delimiters and the byte ranges
/// of string/comment nodes. All bracket types (`{`/`(`/`[` and their closers)
/// are tracked. The walk does not descend into protected nodes, so delimiters
/// inside strings/comments never count toward depth. Per-language switch/case
/// adjustments are collected in a second pass via [`collect_case_adjustments`].
fn collect_cst(root: &Node, grammar_name: &str) -> CstFacts {
    let mut delimiters = Vec::new();
    let mut protected: Vec<(usize, usize)> = Vec::new();
    let mut cursor = root.walk();
    loop {
        let node = cursor.node();
        let kind = node.kind();
        let is_protected = kind.contains("string") || kind.contains("comment");
        if is_protected {
            protected.push((node.start_byte(), node.end_byte()));
        } else if node.child_count() == 0 {
            match kind.as_str() {
                "(" | "[" | "{" => delimiters.push(Delimiter {
                    byte: node.start_byte(),
                    open: true,
                }),
                ")" | "]" | "}" => delimiters.push(Delimiter {
                    byte: node.start_byte(),
                    open: false,
                }),
                _ => {}
            }
        }
        if !is_protected && cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                delimiters.sort_by_key(|d| d.byte);
                protected.sort_by_key(|r| r.0);
                let mut case_label_dedent = Vec::new();
                let mut case_body_extra = Vec::new();
                collect_case_adjustments(root, grammar_name, &mut case_body_extra, &mut case_label_dedent);
                return CstFacts {
                    delimiters,
                    protected,
                    case_label_dedent,
                    case_body_extra,
                };
            }
        }
    }
}

/// Populate per-language switch/case indent-adjustment ranges. This is a
/// second CST pass kept separate from the main delimiter walk for clarity.
fn collect_case_adjustments(
    root: &Node,
    grammar_name: &str,
    case_body_extra: &mut Vec<(usize, usize)>,
    case_label_dedent: &mut Vec<(usize, usize)>,
) {
    match grammar_name {
        "swift" => collect_swift_case_adjustments(root, case_body_extra, case_label_dedent),
        "dart" => collect_switch_case_bodies(
            root,
            "switch_statement_case",
            "switch_statement_default",
            case_body_extra,
        ),
        "csharp" => collect_switch_case_bodies(root, "switch_section", "switch_section", case_body_extra),
        _ => {}
    }
}

/// Swift: each `switch_entry` node is dedented back to the `switch` level
/// (swift-format's style), and its `statements` child is given extra indent so
/// the body ends up at bracket depth (net adjustment of 0 for body lines).
fn collect_swift_case_adjustments(
    root: &Node,
    case_body_extra: &mut Vec<(usize, usize)>,
    case_label_dedent: &mut Vec<(usize, usize)>,
) {
    if root.kind() == "switch_entry" {
        case_label_dedent.push((root.start_byte(), root.end_byte()));
        let mut cursor = root.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                if child.kind() == "statements" {
                    case_body_extra.push((child.start_byte(), child.end_byte()));
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }
    let mut cursor = root.walk();
    if cursor.goto_first_child() {
        loop {
            collect_swift_case_adjustments(&cursor.node(), case_body_extra, case_label_dedent);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

/// Dart / C#: walk the tree looking for `case_kind` and `default_kind` nodes.
/// For each such node, find the last `:` child; any siblings after that `:` are
/// the implicit case body and receive a +1 extra-indent adjustment.
fn collect_switch_case_bodies(
    root: &Node,
    case_kind: &str,
    default_kind: &str,
    case_body_extra: &mut Vec<(usize, usize)>,
) {
    let kind = root.kind();
    if kind == case_kind || kind == default_kind {
        let mut after_colon = false;
        let mut cursor = root.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                if after_colon {
                    case_body_extra.push((child.start_byte(), child.end_byte()));
                }
                if child.kind() == ":" {
                    after_colon = true;
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }
    let mut cursor = root.walk();
    if cursor.goto_first_child() {
        loop {
            collect_switch_case_bodies(&cursor.node(), case_kind, default_kind, case_body_extra);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

/// The indent unit string for a grammar: a tab for Go (gofmt), two spaces for
/// Dart (`dart format`), Swift (Xcode default is 4 but swift-format and the
/// conformance golden use 2), and Elixir (`mix format` uses 2-space canonical
/// style), and `indent_width` spaces (default 4) otherwise.
fn indent_unit(grammar_name: &str, indent_width: usize) -> Cow<'static, str> {
    match grammar_name {
        "go" => Cow::Borrowed("\t"),
        "dart" | "swift" | "elixir" => Cow::Borrowed("  "),
        _ => Cow::Owned(" ".repeat(indent_width.max(1))),
    }
}

/// Re-emit `source` with each line indented by its bracket depth. Uses the
/// conservative level-keyed-by-open-line model: multiple brackets opened on the
/// same line coalesce to one level; leading closers release their levels before
/// the render depth is sampled. Verbatim emission for protected ranges.
fn reindent(source: &str, grammar_name: &str, facts: &CstFacts, cfg: &EngineConfig) -> String {
    let unit = indent_unit(grammar_name, cfg.indent_width);
    let line_ending = cfg.globals.line_ending.as_str();
    let delimiters = &facts.delimiters;

    const MAX_PRECOMPUTED_DEPTH: usize = 64;
    let max_indent = unit.repeat(MAX_PRECOMPUTED_DEPTH);

    let mut out = String::with_capacity(source.len() + source.len() / 8);
    let mut byte = 0usize;
    let mut first = true;
    let mut raw_stack: Vec<usize> = Vec::new();
    let mut active_levels: Vec<usize> = Vec::new();
    let mut d_cursor = 0usize;

    for (line_idx, raw) in source.split('\n').enumerate() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let line_start = byte;
        let line_end = byte + line.len();

        if facts.is_interior(line_start) {
            if !first {
                out.push_str(line_ending);
            }
            first = false;
            out.push_str(line);
            byte += raw.len() + 1;
            continue;
        }

        let d_line_start = d_cursor;
        while d_cursor < delimiters.len() && delimiters[d_cursor].byte < line_end {
            d_cursor += 1;
        }
        let line_delims = &delimiters[d_line_start..d_cursor];

        let leading = count_leading_closers(line, line_start, line_delims);
        for _ in 0..leading {
            if let Some(open_line) = raw_stack.pop()
                && let Some(pos) = active_levels.iter().position(|&x| x == open_line)
            {
                active_levels.remove(pos);
            }
        }

        let base = active_levels.len() as i32;
        let level = (base + facts.case_adjustment(line_start)).max(0) as usize;

        if !first {
            out.push_str(line_ending);
        }
        first = false;
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

        for d in &line_delims[leading..] {
            if d.open {
                raw_stack.push(line_idx);
            } else if let Some(open_line) = raw_stack.pop()
                && let Some(pos) = active_levels.iter().position(|&x| x == open_line)
            {
                active_levels.remove(pos);
            }
        }

        if raw_stack.contains(&line_idx) && !active_levels.contains(&line_idx) {
            active_levels.push(line_idx);
        }

        byte += raw.len() + 1;
    }

    apply_trailing_newline(&mut out, source, line_ending, cfg.globals.final_newline);
    out
}

/// Count consecutive closing-bracket CST tokens at the start of `line` (after
/// whitespace). Stops at the first character that is not `)`, `]`, `}`, or
/// that does not correspond to a real CST closer token in `line_delims`.
/// `line_delims` must be sorted by byte (ascending) — inherited from the sort
/// in [`collect_cst`].
fn count_leading_closers(line: &str, line_start: usize, line_delims: &[Delimiter]) -> usize {
    let Some(first_nonws) = line.find(|c: char| !c.is_whitespace()) else {
        return 0;
    };
    let mut count = 0usize;
    let mut abs = line_start + first_nonws;
    for ch in line[first_nonws..].chars() {
        match ch {
            ')' | ']' | '}' => {
                let is_real_closer = line_delims
                    .binary_search_by_key(&abs, |d| d.byte)
                    .is_ok_and(|i| !line_delims[i].open);
                if is_real_closer {
                    count += 1;
                    abs += ch.len_utf8();
                } else {
                    break;
                }
            }
            _ => break,
        }
    }
    count
}

/// Ensure the output ends (or does not end) with a single trailing newline,
/// mirroring the configured `final_newline` policy and the original source.
fn apply_trailing_newline(out: &mut String, source: &str, line_ending: &str, final_newline: bool) {
    while out.ends_with('\n') || out.ends_with('\r') {
        out.pop();
    }
    if final_newline && !source.is_empty() {
        out.push_str(line_ending);
    }
}
#[cfg(test)]
mod tests;
