//! Tier-2 generic formatter: the catch-all backend for every language without
//! a native crate backend. Built on `tree-sitter-language-pack`, which fetches
//! and dynamically loads grammars on demand, so one binary covers the long tail
//! of languages with zero preinstalled language tools.
//!
//! Two modes, chosen per language:
//! - **Structural reindent** for brace-delimited grammars (Go, C, C++, Java,
//!   Kotlin, Rust, …): the CST locates the real `{}` / `[]` / `()` delimiter
//!   tokens — ignoring any that live inside strings or comments, since those are
//!   not separate leaf nodes — and each line is re-indented by bracket depth.
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
        "1"
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
                    fix: None,
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
        if formatted == src.content {
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
        let delimiters = collect_delimiters(&tree.root_node());
        Some(reindent(&src.content, &delimiters, cfg))
    })
}

/// A delimiter token located in the CST: its byte offset and whether it opens.
struct Delimiter {
    byte: usize,
    open: bool,
}

/// Collect every brace/bracket/paren delimiter that is a real leaf token in the
/// CST, in source order. Delimiters inside string or comment nodes are not
/// separate leaves, so they are naturally excluded.
fn collect_delimiters(root: &Node) -> Vec<Delimiter> {
    let mut out = Vec::new();
    let mut cursor = root.walk();
    loop {
        let node = cursor.node();
        if node.child_count() == 0 {
            match node.kind().as_str() {
                "{" | "(" | "[" => out.push(Delimiter {
                    byte: node.start_byte(),
                    open: true,
                }),
                "}" | ")" | "]" => out.push(Delimiter {
                    byte: node.start_byte(),
                    open: false,
                }),
                _ => {}
            }
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                out.sort_by_key(|d| d.byte);
                return out;
            }
        }
    }
}

/// Re-emit `source` with each line indented by its bracket depth. A line that
/// begins with a closing delimiter is dedented one level. Blank lines are
/// preserved as empty. Trailing whitespace is stripped and the configured line
/// ending / final newline are applied.
fn reindent(source: &str, delimiters: &[Delimiter], cfg: &EngineConfig) -> String {
    let unit = " ".repeat(cfg.indent_width.max(1));
    let line_ending = cfg.globals.line_ending.as_str();

    let mut out = String::with_capacity(source.len() + source.len() / 8);
    let mut depth: i32 = 0;
    let mut byte = 0usize;
    let mut first = true;

    for raw in source.split('\n') {
        // Strip a trailing '\r' so CRLF input is handled; re-added via line_ending.
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let line_start = byte;
        let line_end = byte + line.len();

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
            content: content.to_string(),
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
