//! Tier-2 generic formatter: the catch-all backend for every language without
//! a native crate backend. Built on `tree-sitter-language-pack`, which fetches
//! and dynamically loads grammars on demand, so one binary covers the long tail
//! of languages with zero preinstalled language tools.
//!
//! Two modes, chosen per language:
//! - **Structural reindent** for brace-delimited grammars (Go, C, C++, Java,
//!   Kotlin, Rust, …): the CST locates the real `{}` / `[]` / `()` delimiter
//!   tokens and re-indents each line by bracket depth. Byte ranges covered by
//!   string-literal and comment CST nodes are excluded: delimiters inside them
//!   never count toward depth, and any line whose leading whitespace begins
//!   inside such a range is emitted verbatim — so the interior of a multiline
//!   raw string, heredoc, or block comment is byte-preserved. The indent unit
//!   is per-grammar (tabs for Go, two spaces for Swift/Dart, `indent_width`
//!   spaces otherwise) to match each language's canonical formatter.
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
        // Bumped to 2: string/comment ranges are now excluded from reindent and
        // the indent unit is per-grammar, so cached output could differ from v1.
        "2"
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
        let cst = collect_cst(&tree.root_node());
        Some(reindent(&src.content, name, &cst, cfg))
    })
}

/// A delimiter token located in the CST: its byte offset and whether it opens.
struct Delimiter {
    byte: usize,
    open: bool,
}

/// Structural facts extracted from one CST walk: the brace/bracket/paren
/// delimiters that drive depth, and the byte ranges of string/comment nodes
/// whose interiors must never be reindented.
struct CstFacts {
    /// Delimiters in source order. Excludes any inside a protected range.
    delimiters: Vec<Delimiter>,
    /// `[start, end)` byte ranges covered by string-literal / comment nodes,
    /// in source order. Used to leave string and comment interiors verbatim.
    protected: Vec<(usize, usize)>,
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
}

/// Walk the CST once, collecting depth-driving delimiters and the byte ranges of
/// string/comment nodes. tree-sitter node kinds vary per grammar, so protected
/// nodes are matched by `kind()` containing "string" or "comment" — the portable
/// signal across grammars. The walk does not descend into a protected node, so
/// delimiters living inside a string or comment are never collected and thus
/// never count toward bracket depth.
fn collect_cst(root: &Node) -> CstFacts {
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
                "{" | "(" | "[" => delimiters.push(Delimiter {
                    byte: node.start_byte(),
                    open: true,
                }),
                "}" | ")" | "]" => delimiters.push(Delimiter {
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
                return CstFacts {
                    delimiters,
                    protected,
                };
            }
        }
    }
}

/// The indent unit string for a grammar: a tab for Go (gofmt indents with
/// tabs), two spaces for Dart (`dart format` defaults to two-space and is
/// near-universal), and `indent_width` spaces (default 4) otherwise. Swift is
/// intentionally left at `indent_width`: although swift-format defaults to two
/// spaces, the dominant Swift idiom (Xcode default) is four, so forcing two
/// churns more real code than it fixes until a native Swift formatter exists.
fn indent_unit(grammar_name: &str, indent_width: usize) -> String {
    match grammar_name {
        "go" => "\t".to_string(),
        "dart" => "  ".to_string(),
        _ => " ".repeat(indent_width.max(1)),
    }
}

/// Re-emit `source` with each line indented by its bracket depth. A line that
/// begins with a closing delimiter is dedented one level. Blank lines are
/// preserved as empty. Trailing whitespace is stripped and the configured line
/// ending / final newline are applied.
///
/// Lines whose leading whitespace begins inside a string-literal or comment
/// range (per `facts.protected`) are emitted **verbatim** — neither reindented
/// nor trimmed — so the interior of a multiline string or block comment is
/// byte-preserved. The indent unit is chosen per grammar via [`indent_unit`].
fn reindent(source: &str, grammar_name: &str, facts: &CstFacts, cfg: &EngineConfig) -> String {
    let unit = indent_unit(grammar_name, cfg.indent_width);
    let line_ending = cfg.globals.line_ending.as_str();
    let delimiters = &facts.delimiters;

    let mut out = String::with_capacity(source.len() + source.len() / 8);
    let mut depth: i32 = 0;
    let mut byte = 0usize;
    let mut first = true;

    for raw in source.split('\n') {
        // Strip a trailing '\r' so CRLF input is handled; re-added via line_ending.
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let line_start = byte;
        let line_end = byte + line.len();

        // A line whose start byte is interior to a string/comment node is part
        // of a multiline literal or block comment: emit it exactly as-is.
        if facts.is_interior(line_start) {
            if !first {
                out.push_str(line_ending);
            }
            first = false;
            out.push_str(line);
            // Delimiters inside protected ranges are not collected, so depth is
            // unaffected here; just advance the byte cursor past the newline.
            byte = line_end + 1;
            continue;
        }

        let opens = count_delimiters(delimiters, line_start, line_end, true);
        let closes = count_delimiters(delimiters, line_start, line_end, false);

        let trimmed = line.trim();
        let starts_with_closer = first_nonws_is_closer(line, line_start, delimiters);
        let level = if starts_with_closer {
            (depth - 1).max(0)
        } else {
            depth.max(0)
        };

        if !first {
            out.push_str(line_ending);
        }
        first = false;
        if !trimmed.is_empty() {
            for _ in 0..level {
                out.push_str(&unit);
            }
            out.push_str(trimmed);
        }

        depth = (depth + opens - closes).max(0);
        byte = line_end + 1; // advance past the '\n'
    }

    apply_trailing_newline(&mut out, source, line_ending, cfg.globals.final_newline);
    out
}

/// Count opening (`open == true`) or closing delimiters whose byte offset falls
/// within `[start, end)`.
fn count_delimiters(delimiters: &[Delimiter], start: usize, end: usize, open: bool) -> i32 {
    delimiters
        .iter()
        .filter(|d| d.open == open && d.byte >= start && d.byte < end)
        .count() as i32
}

/// Whether the first non-whitespace byte of `line` is a closing-delimiter token
/// (checked against the CST, so a `}` inside a string does not count).
fn first_nonws_is_closer(line: &str, line_start: usize, delimiters: &[Delimiter]) -> bool {
    let Some(offset) = line.find(|c: char| !c.is_whitespace()) else {
        return false;
    };
    let abs = line_start + offset;
    delimiters.iter().any(|d| !d.open && d.byte == abs)
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
        // The raw string holds irregularly indented lines (8 and 3 spaces) plus
        // a `{` that must NOT count toward bracket depth. Surrounding code is
        // under-indented so we can prove it gets reindented to 4-space depth.
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
}
