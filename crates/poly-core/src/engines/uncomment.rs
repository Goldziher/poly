//! Opt-in comment-removal lint backend, wrapping the `uncomment` crate.
//!
//! `uncomment` uses tree-sitter to find comments and, guided by preservation
//! rules (shebangs, `~keep`, TODO/FIXME, documentation, user patterns), decides
//! which are removable. This backend surfaces each removable comment as a
//! [`Severity::Warning`] [`Diagnostic`] carrying a delete-the-range [`Edit`], so
//! `poly lint` *reports* removable comments and `poly lint --fix` *strips* them
//! through the runner's normal autofix loop.
//!
//! # Cross-cutting, like `typos`
//!
//! Declared for zero languages (`languages() == &[]`); the registry appends it to
//! every language so any file `uncomment` recognizes (by extension) is covered.
//! Languages it does not recognize are a silent no-op — never an error.
//!
//! # Opt-in
//!
//! **Off by default.** It runs only when `[lint.uncomment] enabled = true` (or a
//! per-language `[lint.<lang>.uncomment] enabled = true`). The gate lives inside
//! [`lint`](UncommentEngine::lint), matching the `native_tool` pattern: the engine
//! always advertises the `lint` capability but returns no findings when disabled.
//!
//! # Configuration
//!
//! The resolved `[lint.uncomment]` options table (global merged with the
//! per-language override, see [`crate::config::Config::engine_config`]) is mapped
//! onto `uncomment`'s `ResolvedConfig`:
//! `enabled`, `remove_todos`, `remove_fixme`, `remove_docs`, `use_default_ignores`
//! (bools) and `preserve_patterns` (string array). The whole table is folded into
//! the lint cache key, so a config change re-runs the engine.
//!
//! # `code_only` (default `true`)
//!
//! The wrapped crate treats *every* non-preserved comment as removable, which
//! false-positives on machine-generated headers (`# alef:hash:…`), prose NOTE
//! blocks and `key = value` directive comments. With `code_only = true` (the
//! default) a removal is kept only when the comment looks like commented-out
//! *code* (see `looks_like_commented_out_code`); set `code_only = false` to
//! restore the strip-every-comment behaviour.

use std::cell::RefCell;
use std::collections::BTreeMap;

use uncomment::Processor;
use uncomment::config::ResolvedConfig;

use crate::config::EngineConfig;
use crate::engine::{Capabilities, Diagnostic, Edit, Engine, Severity, SourceFile, Span};
use crate::language::Language;

/// Cache-key version: the wrapped crate version plus a marker for this backend's
/// own mapping logic. Bump whenever `uncomment` is updated OR the diagnostic/edit
/// mapping below changes (either alters output and must bust the cache).
const UNCOMMENT_VERSION: &str = "uncomment-3.5.1+map2-codeonly";

thread_local! {
    /// One `Processor` per rayon worker thread. The processor owns a reusable
    /// tree-sitter `Parser` (re-`set_language`d per file) and the language
    /// registry, so we never build a parser per file on the hot path.
    static PROCESSOR: RefCell<Processor> = RefCell::new(Processor::new());
}

/// Opt-in tree-sitter comment-removal backend. See the module docs.
pub struct UncommentEngine;

impl Engine for UncommentEngine {
    fn name(&self) -> &'static str {
        "uncomment"
    }

    fn languages(&self) -> &'static [Language] {
        &[]
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            lint: true,
            format: false,
            fix: true,
        }
    }

    fn version(&self) -> &str {
        UNCOMMENT_VERSION
    }

    fn lint(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        if !enabled(cfg) {
            return Ok(Vec::new());
        }

        let resolved = resolved_config(cfg);
        let removals =
            PROCESSOR.with(|processor| processor.borrow_mut().plan_removals(&src.content, &src.path, &resolved));

        let removals = match removals {
            Ok(removals) => removals,
            Err(error) => {
                tracing::debug!(path = %src.path.display(), "uncomment skipped: {error:#}");
                return Ok(Vec::new());
            }
        };

        // By default only genuinely *commented-out code* is reported. The wrapped
        // crate flags every non-preserved comment, which false-positives on
        // machine-generated headers (`# alef:hash:…`), prose NOTE blocks and
        // `key = value` directive comments. `code_only = false` restores the
        // strip-every-comment behaviour for callers that want it.
        let code_only = flag(cfg, "code_only", true);
        let removals: Vec<_> = removals
            .into_iter()
            .filter(|removal| {
                !code_only || {
                    let text = src
                        .content
                        .get(removal.comment_start..removal.comment_end)
                        .unwrap_or("");
                    looks_like_commented_out_code(text)
                }
            })
            .collect();

        let diagnostics = removals
            .into_iter()
            .map(|removal| {
                let code = if removal.is_documentation {
                    "doc-comment"
                } else {
                    "comment"
                };
                Diagnostic {
                    engine: "uncomment".to_owned(),
                    code: Some(code.to_owned()),
                    severity: Severity::Warning,
                    title: "comment can be removed".to_owned(),
                    description: (!removal.preview.is_empty()).then(|| removal.preview.clone()),
                    span: Some(span_of(&src.content, removal.comment_start, removal.comment_end)),
                    url: None,
                    fix: vec![Edit {
                        start_byte: removal.remove_start,
                        end_byte: removal.remove_end,
                        replacement: String::new(),
                    }],
                    metadata: BTreeMap::new(),
                }
            })
            .collect();
        Ok(diagnostics)
    }
}

/// Whether `[lint.uncomment] enabled` (merged with the per-language override) is
/// `true`. Defaults to `false` — the backend is opt-in.
fn enabled(cfg: &EngineConfig) -> bool {
    flag(cfg, "enabled", false)
}

/// Read a boolean option from the merged `[lint.uncomment]` table, falling back
/// to `default` when it is absent or not a bool.
fn flag(cfg: &EngineConfig, key: &str, default: bool) -> bool {
    cfg.options.get(key).and_then(toml::Value::as_bool).unwrap_or(default)
}

/// Machine-generated / directive comment prefixes that must never be reported as
/// removable code. Matched case-insensitively against the marker-stripped text.
const MACHINE_HEADER_PREFIXES: &[&str] = &[
    "alef:",
    "re-generate with",
    "regenerate with",
    "generated by",
    "generated with",
    "code generated",
    "auto-generated",
    "autogenerated",
    "auto generated",
    "do not edit",
    "@generated",
    "sourcehash",
    "checksum",
];

/// Code keywords that, appearing as the first token of a line, are strong
/// evidence the line is commented-out code rather than prose.
const CODE_KEYWORDS: &[&str] = &[
    "let",
    "const",
    "var",
    "fn",
    "func",
    "def",
    "return",
    "import",
    "from",
    "use",
    "pub",
    "class",
    "struct",
    "enum",
    "impl",
    "if",
    "else",
    "for",
    "while",
    "match",
    "switch",
    "case",
    "public",
    "private",
    "protected",
    "static",
    "void",
    "package",
    "type",
    "interface",
    "async",
    "await",
    "throw",
    "throws",
    "new",
    "export",
    "module",
    "trait",
    "namespace",
    "using",
    "print",
    "println",
    "echo",
    "assert",
    "yield",
    "raise",
    "with",
    "lambda",
];

/// Code-specific operator substrings that prose and `key = value` directives do
/// not contain. A bare `=` or `:` is deliberately excluded — those appear in
/// `key = value` / `key: value` config directives we must not flag.
const CODE_OPERATORS: &[&str] = &[
    "::", "->", "=>", "&&", "||", "==", "!=", ">=", "<=", "+=", "-=", "*=", "/=",
];

/// Whether a comment (marker included, possibly multi-line) looks like
/// commented-out *code* rather than prose, a machine-generated header, or a
/// `key = value` directive. Conservative by design: a comment is reported only
/// when at least one of its lines carries a strong code signal, so the opt-in
/// lint under-flags rather than resurrecting the false positives it exists to
/// remove.
fn looks_like_commented_out_code(comment: &str) -> bool {
    comment.lines().any(|line| line_is_code(strip_comment_markers(line)))
}

/// Strip leading/trailing comment punctuation (`// # ; * <!-- --> /* */ -- %`)
/// and surrounding whitespace, leaving the candidate source text.
fn strip_comment_markers(line: &str) -> &str {
    let line = line.trim();
    let line = line
        .trim_start_matches(['/', '#', '*', ';', '<', '!', '-', '%', ' ', '\t'])
        .trim_end_matches(['*', '/', '>', '-', ' ', '\t']);
    line.trim()
}

/// Classify a single marker-stripped line. Rejects empties, machine headers and
/// prose sentences up front, then requires a positive code signal.
fn line_is_code(line: &str) -> bool {
    if line.is_empty() || is_machine_header(line) || is_prose_sentence(line) {
        return false;
    }
    has_code_signal(line)
}

/// A generator/directive header such as `alef:hash:…`, `Code generated … DO NOT
/// EDIT`, or a `<word>:hash:` sourcehash line.
fn is_machine_header(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    if lower.contains(":hash:") {
        return true;
    }
    MACHINE_HEADER_PREFIXES.iter().any(|prefix| lower.starts_with(prefix))
}

/// A natural-language sentence: ends in `.`, `!` or `?` and is mostly
/// alphabetic words. Real commented-out code ends in `;`, `)` or `}`, so this
/// only rejects prose.
fn is_prose_sentence(line: &str) -> bool {
    if !line.ends_with(['.', '!', '?']) {
        return false;
    }
    let words: Vec<&str> = line.split_whitespace().collect();
    if words.len() < 3 {
        return false;
    }
    let wordish = words
        .iter()
        .filter(|word| {
            word.chars()
                .all(|c| c.is_ascii_alphabetic() || matches!(c, '.' | ',' | '\'' | '-' | '!' | '?'))
        })
        .count();
    // per-word alphabetic test (parentheses, `;`) and stay classified as code.
    wordish * 10 >= words.len() * 7
}

/// Whether the line carries a high-confidence code signal. Bare `=`/`:` are
/// intentionally *not* signals so `key = value` directive comments are rejected.
fn has_code_signal(line: &str) -> bool {
    if line.ends_with([';', '{', '}']) {
        return true;
    }
    if CODE_OPERATORS.iter().any(|op| line.contains(op)) {
        return true;
    }
    if has_call_shape(line) {
        return true;
    }
    first_token_is_keyword(line)
}

/// An identifier immediately followed by `(` (no space) — a call or definition,
/// e.g. `print(`. The no-space rule avoids matching prose parentheticals like
/// `repo (src/...)`.
fn has_call_shape(line: &str) -> bool {
    let bytes = line.as_bytes();
    for (index, &byte) in bytes.iter().enumerate() {
        if byte == b'(' && index > 0 {
            let prev = bytes[index - 1];
            if prev == b'_' || prev.is_ascii_alphanumeric() {
                return true;
            }
        }
    }
    false
}

/// Whether the first whitespace-delimited token (trailing punctuation trimmed)
/// is a known code keyword.
fn first_token_is_keyword(line: &str) -> bool {
    let Some(first) = line.split_whitespace().next() else {
        return false;
    };
    let token = first.trim_end_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_');
    CODE_KEYWORDS.contains(&token)
}

/// Map the resolved `[lint.uncomment]` options table onto `uncomment`'s
/// `ResolvedConfig`. `respect_gitignore` / `traverse_git_repos` are irrelevant
/// here — poly owns file discovery — and `language_config` is `None` so the
/// crate's built-in registry supplies comment node types by extension.
fn resolved_config(cfg: &EngineConfig) -> ResolvedConfig {
    let flag = |key: &str, default: bool| cfg.options.get(key).and_then(toml::Value::as_bool).unwrap_or(default);
    let preserve_patterns = cfg
        .options
        .get("preserve_patterns")
        .and_then(toml::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();

    ResolvedConfig {
        remove_todos: flag("remove_todos", false),
        remove_fixme: flag("remove_fixme", false),
        remove_docs: flag("remove_docs", false),
        preserve_patterns,
        use_default_ignores: flag("use_default_ignores", true),
        respect_gitignore: true,
        traverse_git_repos: false,
        language_config: None,
    }
}

/// Build a 1-based [`Span`] covering the byte range `[start, end)` of `content`.
fn span_of(content: &str, start: usize, end: usize) -> Span {
    let (start_line, start_col) = line_col(content, start);
    let (end_line, end_col) = line_col(content, end);
    Span {
        start_line,
        start_col,
        end_line,
        end_col,
    }
}

/// Convert a byte offset into `content` to a 1-based (line, column) pair. Columns
/// are counted in bytes, matching the convention used elsewhere in poly.
fn line_col(content: &str, offset: usize) -> (u32, u32) {
    let offset = offset.min(content.len());
    let mut line: u32 = 1;
    let mut col: u32 = 1;
    for &byte in &content.as_bytes()[..offset] {
        if byte == b'\n' {
            line = line.saturating_add(1);
            col = 1;
        } else {
            col = col.saturating_add(1);
        }
    }
    (line, col)
}

#[cfg(test)]
mod tests {
    use super::looks_like_commented_out_code;

    #[test]
    fn flags_commented_out_rust_statement() {
        assert!(looks_like_commented_out_code("// let x = foo();"));
        assert!(looks_like_commented_out_code("// return value;"));
    }

    #[test]
    fn flags_commented_out_python_call() {
        assert!(looks_like_commented_out_code("# print(\"debug\")"));
    }

    #[test]
    fn rejects_alef_hash_header() {
        assert!(!looks_like_commented_out_code(
            "# alef:hash:cba947bdd989e2d5af4d9d0d92fa7d3024ad0b0bd1184fb400bee8d671468c90"
        ));
    }

    #[test]
    fn rejects_regenerate_directive() {
        assert!(!looks_like_commented_out_code("# Re-generate with: alef scaffold"));
        assert!(!looks_like_commented_out_code(
            "# This file is auto-generated by alef. DO NOT EDIT."
        ));
    }

    #[test]
    fn rejects_key_value_directive() {
        assert!(!looks_like_commented_out_code("# key = value"));
        assert!(!looks_like_commented_out_code("# indent_size = 4"));
    }

    #[test]
    fn rejects_multiline_english_prose() {
        let note = "# Required for PyO3 / ext-php-rs cdylibs: Python and Zend C-API symbols are\n\
            # resolved at runtime when the host loads the extension, not at link time.\n\
            # macOS ld is strict and rejects unresolved symbols by default.";
        assert!(!looks_like_commented_out_code(note));
    }

    #[test]
    fn rejects_yaml_prose_comment_with_globs() {
        let comment = "# Dependabot only manages the hand-authored root manifests. Alef-generated trees\n\
            # (packages/**, e2e/**, crates/*-{node,wasm,...}; see .gitattributes) are overwritten\n\
            # by `alef build`, so their dependency versions are maintained upstream in the alef";
        assert!(!looks_like_commented_out_code(comment));
    }

    #[test]
    fn keeps_code_inside_a_mostly_prose_block() {
        // A block whose last line is real commented-out code must still be flagged.
        let block = "# increment the counter before returning\n# counter += 1;";
        assert!(looks_like_commented_out_code(block));
    }
}
