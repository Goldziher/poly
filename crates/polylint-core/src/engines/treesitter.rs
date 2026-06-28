//! Tier-2 generic formatter: the catch-all backend for every language without
//! a native crate backend. Built on `tree-sitter-language-pack`, which fetches
//! and dynamically loads grammars on demand, so one binary covers the long tail
//! of languages with zero preinstalled language tools.
//!
//! Two modes, chosen per language:
//! - **Structural reindent** for brace-delimited grammars (Go, C, C++, Java,
//!   Kotlin, Rust, …): the CST locates all bracket tokens and re-indents each
//!   line by depth using a **conservative level-keyed-by-open-line** model.
//!   Multiple brackets opened on the same line coalesce to one indent level.
//!   A level is released when the first leading closer pops any bracket opened
//!   on that line. Byte ranges covered by string-literal and comment CST nodes
//!   are excluded: delimiters inside them never count toward depth, and any
//!   line whose leading whitespace begins inside such a range is emitted
//!   verbatim. The indent unit is per-grammar (tabs for Go, two spaces for
//!   Swift/Dart, `indent_width` spaces otherwise). Per-language switch/case
//!   adjustments apply for Swift (case labels align with `switch`), Dart, and
//!   C# (case bodies get an extra indent level).
//! - **Whitespace normalization** for every other grammar, and whenever the
//!   grammar is unavailable or the source fails to parse. This never corrupts
//!   unparsable input (it only trims trailing whitespace and fixes line
//!   endings / the final newline).

use std::cell::RefCell;
use std::collections::HashMap;

use tree_sitter_language_pack::{Node, Parser, detect_language, get_parser};

use crate::config::EngineConfig;
use crate::defaults::normalize_whitespace;
use crate::engine::{Capabilities, Diagnostic, Engine, FormatOutput, Severity, SourceFile, Span};
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
    "go", "c", "cpp", "java", "kotlin", "rust", "scala", "swift", "php", "csharp", "objc", "proto",
    "dart", "glsl", "hlsl", "cuda", "zig",
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
            lint: true,
            format: true,
            fix: true,
        }
    }

    fn version(&self) -> &str {
        // Version 5: replaced brace-line-dominance with the conservative
        // level-keyed-by-open-line model. Multiple brackets opened on the same
        // line coalesce to one indent level; a level is released by the first
        // leading closer that pops any bracket from that open-line.
        // Also carries the CRLF byte-cursor fix from v4.
        "5"
    }

    fn lint(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        // Language-agnostic trailing-whitespace lint, the catch-all diagnostic
        // for every file the generic tier serves.
        if !cfg.globals.trim_trailing_whitespace {
            return Ok(Vec::new());
        }
        let mut diags = Vec::new();
        for (i, raw) in src.content.split('\n').enumerate() {
            let line = raw.strip_suffix('\r').unwrap_or(raw);
            let trimmed_len = line.trim_end().len();
            if trimmed_len != line.len() {
                diags.push(Diagnostic {
                    engine: "treesitter".to_string(),
                    code: Some("trailing-whitespace".to_string()),
                    severity: Severity::Warning,
                    message: "trailing whitespace".to_string(),
                    span: Some(Span {
                        start_line: (i + 1) as u32,
                        start_col: (trimmed_len + 1) as u32,
                        end_line: (i + 1) as u32,
                        end_col: (line.len() + 1) as u32,
                    }),
                    fix: vec![],
                    metadata: Default::default(),
                });
            }
        }
        Ok(diags)
    }

    fn format(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
        let formatted = match grammar_name(src) {
            Some(name) if BRACE_FAMILY.contains(&name.as_str()) => reindent_braces(&name, src, cfg)
                .unwrap_or_else(|| normalize_whitespace(&src.content, &cfg.globals)),
            _ => normalize_whitespace(&src.content, &cfg.globals),
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
        // `contains_key` above guarantees the entry exists.
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
    fn is_interior(&self, byte: usize) -> bool {
        self.protected
            .iter()
            .any(|&(start, end)| start < byte && byte < end)
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
        // Descend only into non-protected nodes; a string/comment subtree is
        // treated as an opaque protected range.
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
                collect_case_adjustments(
                    root,
                    grammar_name,
                    &mut case_body_extra,
                    &mut case_label_dedent,
                );
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
        // Node-kind strings were verified against:
        //   swift  — tree-sitter-swift 0.6.x (grammar tag v0.6.0)
        //   dart   — tree-sitter-dart  0.0.3 (grammar shipped in language-pack)
        //   csharp — tree-sitter-c-sharp 0.23.x (grammar tag v0.23.1)
        // If the grammar is upgraded, re-check that these node kinds still exist.
        "swift" => collect_swift_case_adjustments(root, case_body_extra, case_label_dedent),
        "dart" => collect_switch_case_bodies(
            root,
            "switch_statement_case",
            "switch_statement_default",
            case_body_extra,
        ),
        "csharp" => {
            collect_switch_case_bodies(root, "switch_section", "switch_section", case_body_extra)
        }
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
        // The entire entry (case label + body) is dedented one level so the
        // case label sits at the switch's depth, not the switch body's depth.
        case_label_dedent.push((root.start_byte(), root.end_byte()));
        // The `statements` child gets +1 so it ends at bracket depth (the
        // dedent and the extra-indent cancel, keeping body at bracket depth).
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
    // Always recurse so nested switch statements are handled.
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
    // Recurse so nested switch statements are handled.
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
/// Dart (`dart format`) and Swift (Xcode default is 4 but swift-format and the
/// conformance golden use 2), and `indent_width` spaces (default 4) otherwise.
fn indent_unit(grammar_name: &str, indent_width: usize) -> String {
    match grammar_name {
        "go" => "\t".to_string(),
        "dart" | "swift" => "  ".to_string(),
        _ => " ".repeat(indent_width.max(1)),
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

    let mut out = String::with_capacity(source.len() + source.len() / 8);
    let mut byte = 0usize;
    let mut first = true;
    // raw_stack: per-open-bracket entry storing the line index it was opened on.
    let mut raw_stack: Vec<usize> = Vec::new();
    // active_levels: each distinct open-line contributes at most one depth unit.
    let mut active_levels: Vec<usize> = Vec::new();

    for (line_idx, raw) in source.split('\n').enumerate() {
        // Strip '\r' for CRLF; it is re-added via line_ending.
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let line_start = byte;
        let line_end = byte + line.len();

        // Interior of a multiline string/comment: emit verbatim, don't reindent.
        if facts.is_interior(line_start) {
            if !first {
                out.push_str(line_ending);
            }
            first = false;
            out.push_str(line);
            byte += raw.len() + 1; // raw includes '\r' for CRLF — must not use line.len()
            continue;
        }

        // Delimiters on this line in byte order.
        let line_delims: Vec<&Delimiter> = delimiters
            .iter()
            .filter(|d| d.byte >= line_start && d.byte < line_end)
            .collect();

        // Leading-closer run: consecutive `)` `]` `}` at line start.
        // Each pops raw_stack and, if the popped open-line is in active_levels,
        // releases that level. Depth is sampled AFTER all leading closers fire.
        let leading = count_leading_closers(line, line_start, &line_delims);
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
            for _ in 0..level {
                out.push_str(&unit);
            }
            out.push_str(trimmed);
        }

        // Process remaining delimiters: openers push line_idx, closers pop and
        // optionally release.
        for d in &line_delims[leading..] {
            if d.open {
                raw_stack.push(line_idx);
            } else if let Some(open_line) = raw_stack.pop()
                && let Some(pos) = active_levels.iter().position(|&x| x == open_line)
            {
                active_levels.remove(pos);
            }
        }

        // Coalesce: if this line left unmatched opens, it contributes one new level.
        if raw_stack.contains(&line_idx) && !active_levels.contains(&line_idx) {
            active_levels.push(line_idx);
        }

        byte += raw.len() + 1; // raw includes '\r' for CRLF — must not use line.len()
    }

    apply_trailing_newline(&mut out, source, line_ending, cfg.globals.final_newline);
    out
}

/// Count consecutive closing-bracket CST tokens at the start of `line` (after
/// whitespace). Stops at the first character that is not `)`, `]`, `}`, or
/// that does not correspond to a real CST closer token in `line_delims`.
/// `line_delims` must be sorted by byte (ascending) — inherited from the sort
/// in [`collect_cst`].
fn count_leading_closers(line: &str, line_start: usize, line_delims: &[&Delimiter]) -> usize {
    let Some(first_nonws) = line.find(|c: char| !c.is_whitespace()) else {
        return 0;
    };
    let mut count = 0usize;
    let mut abs = line_start + first_nonws;
    for ch in line[first_nonws..].chars() {
        match ch {
            ')' | ']' | '}' if line_delims.iter().any(|d| !d.open && d.byte == abs) => {
                count += 1;
                abs += ch.len_utf8();
            }
            _ => break,
        }
    }
    count
}

/// Ensure the output ends (or does not end) with a single trailing newline,
/// mirroring the configured `final_newline` policy and the original source.
fn apply_trailing_newline(out: &mut String, source: &str, line_ending: &str, final_newline: bool) {
    // `split('\n')` produced a trailing empty segment for sources that already
    // ended in '\n'; that empty segment added a dangling line ending we trim
    // back here before applying the policy.
    while out.ends_with('\n') || out.ends_with('\r') {
        out.pop();
    }
    if final_newline && !source.is_empty() {
        out.push_str(line_ending);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::config::GlobalDefaults;

    fn cfg(indent_width: usize) -> EngineConfig {
        EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width,
            options: toml::Table::new(),
        }
    }

    fn src(path: &str, language: Language, content: &str) -> SourceFile {
        SourceFile {
            path: PathBuf::from(path),
            language,
            content: content.into(),
        }
    }

    #[test]
    fn metadata_is_generic_lint_and_format() {
        let engine = TreeSitterEngine;
        assert_eq!(engine.name(), "treesitter");
        assert!(engine.languages().is_empty());
        let caps = engine.capabilities();
        assert!(caps.format);
        // The generic tier carries the catch-all trailing-whitespace lint.
        assert!(caps.lint);
    }

    fn formatted_text(out: FormatOutput, original: &str) -> String {
        match out {
            FormatOutput::Formatted(text) => text,
            FormatOutput::Unchanged => original.to_string(),
        }
    }

    #[test]
    fn rust_raw_string_interior_is_byte_preserved_while_code_reindents() {
        // Raw string interior (including `{`) is verbatim; surrounding code reindents.
        let engine = TreeSitterEngine;
        let input = concat!(
            "fn main() {\n",
            "let template = r#\"\n",
            "        deeply indented {line}\n",
            "   another\n",
            "\"#;\n",
            "println!(\"{}\", template);\n",
            "}\n",
        );
        let expected = concat!(
            "fn main() {\n",
            "    let template = r#\"\n",
            "        deeply indented {line}\n",
            "   another\n",
            "\"#;\n",
            "    println!(\"{}\", template);\n",
            "}\n",
        );
        let s = src("main.rs", Language::Other("rust".into()), input);
        let text = formatted_text(engine.format(&s, &cfg(4)).unwrap(), input);
        assert_eq!(text, expected, "code reindented, string interior preserved");
        // The exact interior bytes between the raw-string delimiters survive.
        let interior = "\n        deeply indented {line}\n   another\n";
        assert!(
            text.contains(interior),
            "raw-string interior must be verbatim"
        );
    }

    #[test]
    fn go_reindents_with_tabs_not_spaces() {
        let engine = TreeSitterEngine;
        let input = concat!("package main\n", "\n", "func main() {\n", "x := 1\n", "}\n");
        let expected = concat!(
            "package main\n",
            "\n",
            "func main() {\n",
            "\tx := 1\n",
            "}\n",
        );
        let s = src("main.go", Language::Other("go".into()), input);
        let text = formatted_text(engine.format(&s, &cfg(4)).unwrap(), input);
        assert_eq!(text, expected, "Go must reindent with a tab, not spaces");
    }

    #[test]
    fn whitespace_fallback_for_unknown_language() {
        // An unknown grammar name is never in BRACE_FAMILY, so the engine only
        // normalizes whitespace — no parsing, no grammar download.
        let engine = TreeSitterEngine;
        let s = src(
            "notes.unknownext",
            Language::Other("definitely-not-a-grammar".into()),
            "line with trailing spaces   \nok\n",
        );
        let out = engine.format(&s, &cfg(2)).unwrap();
        match out {
            FormatOutput::Formatted(text) => {
                assert_eq!(text, "line with trailing spaces\nok\n");
            }
            FormatOutput::Unchanged => panic!("expected trailing whitespace to be trimmed"),
        }
    }

    #[test]
    fn swift_uses_two_space_indent() {
        // swift-format defaults to two-space indentation.
        let engine = TreeSitterEngine;
        let input = concat!("struct Point {\n", "let x: Int\n", "let y: Int\n", "}\n");
        let expected = concat!(
            "struct Point {\n",
            "  let x: Int\n",
            "  let y: Int\n",
            "}\n"
        );
        let s = src("test.swift", Language::Other("swift".into()), input);
        let text = formatted_text(engine.format(&s, &cfg(4)).unwrap(), input);
        assert_eq!(text, expected, "Swift must use 2-space indent");
    }

    #[test]
    fn swift_switch_case_labels_align_with_switch_keyword() {
        // swift-format aligns case labels with `switch`, not inside the body.
        // Case label at depth 1 (same as `switch`), body at depth 2.
        let engine = TreeSitterEngine;
        let input = concat!(
            "func f() -> Int {\n",
            "switch shape {\n",
            "case .circle:\n",
            "return 1\n",
            "case .rect:\n",
            "return 2\n",
            "}\n",
            "}\n",
        );
        let expected = concat!(
            "func f() -> Int {\n",
            "  switch shape {\n",
            "  case .circle:\n",
            "    return 1\n",
            "  case .rect:\n",
            "    return 2\n",
            "  }\n",
            "}\n",
        );
        let s = src("test.swift", Language::Other("swift".into()), input);
        let text = formatted_text(engine.format(&s, &cfg(4)).unwrap(), input);
        assert_eq!(
            text, expected,
            "Swift case labels align with switch keyword"
        );
    }

    #[test]
    fn dart_switch_case_body_extra_indent() {
        // dart format indents case bodies one extra level past the case label.
        let engine = TreeSitterEngine;
        let input = concat!(
            "int f(int n) {\n",
            "switch (n) {\n",
            "case 0:\n",
            "return 0;\n",
            "default:\n",
            "return -1;\n",
            "}\n",
            "}\n",
        );
        let expected = concat!(
            "int f(int n) {\n",
            "  switch (n) {\n",
            "    case 0:\n",
            "      return 0;\n",
            "    default:\n",
            "      return -1;\n",
            "  }\n",
            "}\n",
        );
        let s = src("test.dart", Language::Other("dart".into()), input);
        let text = formatted_text(engine.format(&s, &cfg(4)).unwrap(), input);
        assert_eq!(text, expected, "Dart case body gets extra indent level");
    }

    #[test]
    fn dart_closure_argument_not_over_indented() {
        // With the level-keyed-by-open-line model, `list.map((n) {` opens
        // two parens and a brace on the same line — they coalesce to one new
        // depth level (+1). The closure body is therefore at depth+1 (NOT +3),
        // and `})` releases that single level on its closing line.
        let engine = TreeSitterEngine;
        let input = concat!(
            "void main() {\n",
            "final result = list.map((n) {\n",
            "return n * 2;\n",
            "}).toList();\n",
            "}\n",
        );
        let expected = concat!(
            "void main() {\n",
            "  final result = list.map((n) {\n",
            "    return n * 2;\n",
            "  }).toList();\n",
            "}\n",
        );
        let s = src("test.dart", Language::Other("dart".into()), input);
        let text = formatted_text(engine.format(&s, &cfg(4)).unwrap(), input);
        assert_eq!(
            text, expected,
            "Dart closure body must not be over-indented"
        );
    }

    // ── CRLF byte-cursor fix ─────────────────────────────────────────────────

    #[test]
    fn crlf_brace_counting_does_not_drift() {
        // Before the fix, `line.len() + 1` drifted by 1 per line on CRLF,
        // causing delimiters to miss their line window. Fix: `raw.len() + 1`.
        let engine = TreeSitterEngine;
        let crlf = "package main\r\n\r\nfunc main() {\r\nx := 1\r\n}\r\n";
        let lf = "package main\n\nfunc main() {\nx := 1\n}\n";
        let expected = "package main\n\nfunc main() {\n\tx := 1\n}\n";

        let crlf_src = src("main.go", Language::Other("go".into()), crlf);
        let lf_src = src("main.go", Language::Other("go".into()), lf);

        let crlf_out = formatted_text(engine.format(&crlf_src, &cfg(4)).unwrap(), crlf);
        let lf_out = formatted_text(engine.format(&lf_src, &cfg(4)).unwrap(), lf);

        assert_eq!(lf_out, expected, "LF Go reindented with tabs");
        assert_eq!(
            crlf_out, expected,
            "CRLF Go reindented identically (no byte drift)"
        );
    }

    // ── paren/bracket continuation indent ────────────────────────────────────
    // Go/Rust expected outputs verified by running gofmt/rustfmt on the inputs.

    #[test]
    fn go_multiline_call_args_get_continuation_indent() {
        // Ground truth: gofmt. Args one tab deeper than the call site.
        let engine = TreeSitterEngine;
        let input = concat!(
            "package main\n",
            "\n",
            "func main() {\n",
            "result, err := pkg.LongFunc(\n",
            "arg1,\n",
            "arg2,\n",
            ")\n",
            "_ = result\n",
            "_ = err\n",
            "}\n",
        );
        let expected = concat!(
            "package main\n",
            "\n",
            "func main() {\n",
            "\tresult, err := pkg.LongFunc(\n",
            "\t\targ1,\n",
            "\t\targ2,\n",
            "\t)\n",
            "\t_ = result\n",
            "\t_ = err\n",
            "}\n",
        );
        let s = src("main.go", Language::Other("go".into()), input);
        let text = formatted_text(engine.format(&s, &cfg(4)).unwrap(), input);
        assert_eq!(
            text, expected,
            "Go multi-line call args at +1 continuation depth"
        );
    }

    #[test]
    fn rust_multiline_call_args_get_continuation_indent() {
        // Ground truth: rustfmt. Names long enough to stay multi-line at 100-col.
        let engine = TreeSitterEngine;
        let input = concat!(
            "fn main() {\n",
            "let result = some_very_long_function_name(\n",
            "very_long_argument_one,\n",
            "very_long_argument_two,\n",
            "very_long_argument_three,\n",
            ");\n",
            "}\n",
        );
        let expected = concat!(
            "fn main() {\n",
            "    let result = some_very_long_function_name(\n",
            "        very_long_argument_one,\n",
            "        very_long_argument_two,\n",
            "        very_long_argument_three,\n",
            "    );\n",
            "}\n",
        );
        let s = src("main.rs", Language::Other("rust".into()), input);
        let text = formatted_text(engine.format(&s, &cfg(4)).unwrap(), input);
        assert_eq!(
            text, expected,
            "Rust multi-line call args at +1 continuation depth"
        );
    }

    #[test]
    fn java_multiline_call_args_get_continuation_indent() {
        // Expected output from the tier-2 generic reindenter (4-space): each
        // argument is one level deeper than the method body, `)` dedents back.
        let engine = TreeSitterEngine;
        let input = concat!(
            "class Foo {\n",
            "void method() {\n",
            "String result = SomeClass.longMethodName(\n",
            "arg1,\n",
            "arg2,\n",
            "arg3\n",
            ");\n",
            "}\n",
            "}\n",
        );
        let expected = concat!(
            "class Foo {\n",
            "    void method() {\n",
            "        String result = SomeClass.longMethodName(\n",
            "            arg1,\n",
            "            arg2,\n",
            "            arg3\n",
            "        );\n",
            "    }\n",
            "}\n",
        );
        let s = src("Test.java", Language::Other("java".into()), input);
        let text = formatted_text(engine.format(&s, &cfg(4)).unwrap(), input);
        assert_eq!(
            text, expected,
            "Java multi-line call args at +1 continuation depth"
        );
    }

    #[test]
    fn kotlin_multiline_call_args_get_continuation_indent() {
        // Expected output from the tier-2 generic reindenter (4-space): same
        // level-keyed-by-open-line behaviour as Java/Go/Rust.
        let engine = TreeSitterEngine;
        let input = concat!(
            "fun main() {\n",
            "val result = someObject.doTheThing(\n",
            "argument1,\n",
            "argument2,\n",
            ")\n",
            "println(result)\n",
            "}\n",
        );
        let expected = concat!(
            "fun main() {\n",
            "    val result = someObject.doTheThing(\n",
            "        argument1,\n",
            "        argument2,\n",
            "    )\n",
            "    println(result)\n",
            "}\n",
        );
        let s = src("main.kt", Language::Other("kotlin".into()), input);
        let text = formatted_text(engine.format(&s, &cfg(4)).unwrap(), input);
        assert_eq!(
            text, expected,
            "Kotlin multi-line call args at +1 continuation depth"
        );
    }

    // ── regression: level-keyed-by-open-line fixes ───────────────────────────
    // Failed under brace-line-dominance; must pass under the new model.

    #[test]
    fn java_constructor_paren_then_brace_close() {
        // Algorithm-expected (no Java formatter available as ground truth).
        // The `) {` pattern: `)` closes the constructor parameter list while `{`
        // opens the body on the same line. The body must be at class-depth+1 (=2),
        // not class-depth+2 (=3) as the old brace-line-dominance model produced.
        let engine = TreeSitterEngine;
        let input = concat!(
            "class Foo {\n",
            "Foo(\n",
            "Type arg\n",
            ") {\n",
            "this.arg = arg;\n",
            "}\n",
            "}\n",
        );
        let expected = concat!(
            "class Foo {\n",
            "    Foo(\n",
            "        Type arg\n",
            "    ) {\n",
            "        this.arg = arg;\n",
            "    }\n",
            "}\n",
        );
        let s = src("Foo.java", Language::Other("java".into()), input);
        let text = formatted_text(engine.format(&s, &cfg(4)).unwrap(), input);
        assert_eq!(
            text, expected,
            "Java constructor body must be at class+1 depth, not class+2"
        );
    }

    #[test]
    fn go_struct_in_call_close_then_paren_close_no_drift() {
        // Ground truth: `gofmt` on the same input produces the expected output.
        // `doThing(Config{` opens two brackets on one line — they coalesce to one
        // level. After `},` closes the struct, the `(` from `doThing(` is still
        // open at depth 1. The `)` then closes it; code after the call (`x := 1`)
        // must remain at depth 1, not drift to 0.
        let engine = TreeSitterEngine;
        let input = concat!(
            "package main\n",
            "\n",
            "func main() {\n",
            "doThing(Config{\n",
            "field: 1,\n",
            "},\n",
            ")\n",
            "x := 1\n",
            "}\n",
        );
        let expected = concat!(
            "package main\n",
            "\n",
            "func main() {\n",
            "\tdoThing(Config{\n",
            "\t\tfield: 1,\n",
            "\t},\n",
            "\t)\n",
            "\tx := 1\n",
            "}\n",
        );
        let s = src("main.go", Language::Other("go".into()), input);
        let text = formatted_text(engine.format(&s, &cfg(4)).unwrap(), input);
        assert_eq!(
            text, expected,
            "code after struct-in-call must not drift to depth 0"
        );
    }

    #[test]
    fn double_brace_close_releases_two_levels() {
        // Algorithm-expected: `}}` on one line closes two levels opened on two
        // distinct lines, so both are released as leading closers before the
        // render depth is computed, giving depth 0 for the `}}` line itself.
        let engine = TreeSitterEngine;
        let input = concat!("class A {\n", "void f() {\n", "x = 1;\n", "}}\n",);
        let expected = concat!(
            "class A {\n",
            "    void f() {\n",
            "        x = 1;\n",
            "}}\n",
        );
        let s = src("A.java", Language::Other("java".into()), input);
        let text = formatted_text(engine.format(&s, &cfg(4)).unwrap(), input);
        assert_eq!(
            text, expected,
            "}}: two leading closers each release one level"
        );
    }
}
