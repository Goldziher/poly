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
//! *code* (see [`looks_like_commented_out_code`]); set `code_only = false` to
//! restore the strip-every-comment behaviour.
//!
//! # Comment *blocks*, not comment lines
//!
//! Tree-sitter reports one comment node per `//` / `#` line, so upstream returns
//! one [`Removal`] per line. Judging each of those lines independently is how
//! `--fix` came to delete the *interior* line of a three-line prose comment
//! (`// … it always returns `Value::String`. …` — the `::` alone read as code),
//! leaving the surrounding lines welded into a sentence the author never wrote.
//! Removing one line from a block is worse than removing the block: nothing
//! looks missing, so nobody reviews it.
//!
//! [`filter_code_only`] therefore works on **contiguous comment blocks**: a run
//! of whole-line comments with no blank or code line between them. A block is
//! removable only when *every* line in it is covered by a removal (a preserved
//! `TODO` / `~keep` line in the middle vetoes the block) **and** every line reads
//! as commented-out code. Otherwise the whole block is kept. A blank line ends a
//! block — it separates paragraphs, and each paragraph is judged on its own. A
//! trailing comment (`foo(); // note`) shares its line with code, so it is never
//! part of a block and is judged alone.

use std::cell::RefCell;
use std::collections::BTreeMap;

use uncomment::config::ResolvedConfig;
use uncomment::{Processor, Removal};

use crate::config::EngineConfig;
use crate::engine::{Capabilities, Diagnostic, Edit, Engine, Severity, SourceFile, Span};
use crate::language::Language;

/// Cache-key version: the wrapped crate version plus a marker for this backend's
/// own mapping logic. Bump whenever `uncomment` is updated OR the diagnostic/edit
/// mapping below changes (either alters output and must bust the cache).
const UNCOMMENT_VERSION: &str = "uncomment-3.5.2+map2-codeonly+prose3-blocks";

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
        let removals = if flag(cfg, "code_only", true) {
            filter_code_only(&src.content, removals)
        } else {
            removals
        };

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

/// Keep only the removals that belong to a contiguous comment block reading as
/// commented-out code, dropping every removal that would edit a block partially.
///
/// This is the guard described in the module docs: the unit of judgement is the
/// block, never the line. Runs once per file, only when `code_only` is on and
/// upstream found something, so the line index it builds is off the hot path for
/// every file with no removable comment.
fn filter_code_only(content: &str, removals: Vec<Removal>) -> Vec<Removal> {
    if removals.is_empty() {
        return removals;
    }
    let lines = index_lines(content);
    // Unreachable (a removal implies a comment implies a line), but indexing an
    // empty line table on the fix path would panic, so bail instead.
    if lines.is_empty() {
        return Vec::new();
    }

    let mut keep = vec![false; removals.len()];
    let mut spans: Vec<(usize, usize)> = Vec::with_capacity(removals.len());
    // Removal indices grouped by the (first line, last line) of their block.
    let mut blocks: BTreeMap<(usize, usize), Vec<usize>> = BTreeMap::new();

    for (index, removal) in removals.iter().enumerate() {
        let first = line_of(&lines, removal.comment_start);
        let last = line_of(&lines, removal.comment_end.saturating_sub(1).max(removal.comment_start));
        spans.push((first, last));

        let text = content
            .get(removal.comment_start..removal.comment_end)
            .unwrap_or_default();
        let Some(marker) = text.chars().next() else {
            continue;
        };
        if !starts_its_line(&lines, first, removal.comment_start) {
            // A trailing comment sits on a line of code, so no comment block can
            // contain it. Judge it on its own, as before.
            keep[index] = looks_like_commented_out_code(text);
            continue;
        }
        blocks
            .entry(block_bounds(&lines, first, last, marker))
            .or_default()
            .push(index);
    }

    for ((first, last), members) in blocks {
        if block_is_removable(content, &lines, &spans, &members, first, last) {
            for index in members {
                keep[index] = true;
            }
        }
    }

    removals
        .into_iter()
        .zip(keep)
        .filter_map(|(removal, keep)| keep.then_some(removal))
        .collect()
}

/// Whether the whole comment block spanning lines `first..=last` may be removed.
///
/// Both conditions bias towards keeping content: a block partially covered by
/// removals (a preserved `TODO` line inside it) is left alone, and so is a block
/// whose lines are not *unanimously* commented-out code.
fn block_is_removable(
    content: &str,
    lines: &[(usize, &str)],
    spans: &[(usize, usize)],
    members: &[usize],
    first: usize,
    last: usize,
) -> bool {
    let mut covered = vec![false; last - first + 1];
    for &index in members {
        let (start, end) = spans[index];
        for line in start.max(first)..=end.min(last) {
            covered[line - first] = true;
        }
    }
    if !covered.iter().all(|line| *line) {
        return false;
    }

    let (block_start, _) = lines[first];
    let (last_start, last_text) = lines[last];
    let block = content
        .get(block_start..last_start.saturating_add(last_text.len()))
        .unwrap_or_default();
    looks_like_commented_out_code(block)
}

/// Byte offset and newline-free text of every line of `content`, in order.
fn index_lines(content: &str) -> Vec<(usize, &str)> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    for (index, byte) in content.bytes().enumerate() {
        if byte == b'\n' {
            let text = &content[start..index];
            lines.push((start, text.strip_suffix('\r').unwrap_or(text)));
            start = index.saturating_add(1);
        }
    }
    if start < content.len() {
        lines.push((start, &content[start..]));
    }
    lines
}

/// Index of the line containing `offset`. Saturates at the last line so a
/// malformed offset can never panic on the fix path.
fn line_of(lines: &[(usize, &str)], offset: usize) -> usize {
    lines
        .partition_point(|(start, _)| *start <= offset)
        .saturating_sub(1)
        .min(lines.len().saturating_sub(1))
}

/// Whether the comment at `offset` is the first non-whitespace thing on its
/// line — i.e. a whole-line comment rather than a trailing one.
fn starts_its_line(lines: &[(usize, &str)], line: usize, offset: usize) -> bool {
    let (start, text) = lines[line];
    text.get(..offset.saturating_sub(start))
        .is_none_or(|prefix| prefix.trim().is_empty())
}

/// Grow `first..=last` outwards over adjacent whole-line comments sharing the
/// same leading marker character, yielding the contiguous block's bounds.
///
/// Matching on the marker's first character (`/` for both `//` and `///`, `#`
/// for `#` and `#!`) keeps the test language-agnostic and symmetric: two lines
/// must agree on their block, or one of them could be removed alone.
fn block_bounds(lines: &[(usize, &str)], first: usize, last: usize, marker: char) -> (usize, usize) {
    let mut start = first;
    while start > 0 && lines[start - 1].1.trim_start().starts_with(marker) {
        start -= 1;
    }
    let mut end = last;
    while end.saturating_add(1) < lines.len() && lines[end + 1].1.trim_start().starts_with(marker) {
        end += 1;
    }
    (start, end)
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

/// Whether a comment block (markers included, possibly multi-line) looks like
/// commented-out *code* rather than prose, a machine-generated header, or a
/// `key = value` directive.
///
/// **Unanimous**: every non-empty line must read as code, and there must be at
/// least one. Requiring only *one* code-looking line meant a single `::` path in
/// a prose paragraph condemned the paragraph; requiring all of them means a
/// mixed block is kept. That trade is deliberate — a missed removal is cosmetic,
/// a wrong removal silently destroys what the author wrote.
fn looks_like_commented_out_code(comment: &str) -> bool {
    let mut saw_content = false;
    for line in comment.lines() {
        let stripped = strip_comment_markers(line);
        if stripped.is_empty() {
            continue;
        }
        saw_content = true;
        if !line_is_code(stripped) {
            return false;
        }
    }
    saw_content
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
/// prose (both plain sentences and sentences quoting a `::` path) up front, then
/// requires a positive code signal.
fn line_is_code(line: &str) -> bool {
    if line.is_empty() || is_machine_header(line) || is_prose_sentence(line) || is_prose_quoting_a_path(line) {
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
    let words: Vec<&str> = line.split_whitespace().collect();
    let wordish = words.iter().filter(|word| is_wordish(word)).count();

    // A sentence continued across lines does not end in terminal punctuation —
    // it ends mid-clause, often on `;` or `,`. Judging prose by the closing
    // character alone deleted this line from a Helm values file:
    //
    //   # Chromiumoxide engine still gets built because browserEndpoint is set;
    //
    // It ends in `;`, so it was read as commented-out code and `--fix` removed
    // it, leaving the surrounding paragraph still reading as valid English with
    // its explanation silently gone. A long, overwhelmingly word-shaped line is
    // prose regardless of how it ends.
    if words.len() >= PROSE_MIN_WORDS && wordish * 10 >= words.len() * 8 {
        return true;
    }

    // Shorter lines keep the stricter original rule: real commented-out code
    // rarely ends in a full stop.
    if !line.ends_with(['.', '!', '?']) || words.len() < 3 {
        return false;
    }
    // per-word alphabetic test (parentheses, `;`) and stay classified as code.
    wordish * 10 >= words.len() * 7
}

/// Words below this count are too short to judge as prose without terminal
/// punctuation — `return nil, err` would otherwise qualify.
const PROSE_MIN_WORDS: usize = 6;

/// Plain English words a line must carry before a `::` path token in it is read
/// as a *reference to* an item rather than as code. Two is enough to make a
/// sentence (`returns Value::String here`) and still rejects a bare `a::b`.
const PROSE_PATH_MIN_PLAIN_WORDS: usize = 2;

/// Whether the line is an English sentence that happens to name a `::` path —
/// `returns Value::String here`, `see HashMap::new for details`.
///
/// `::` is in [`CODE_OPERATORS`], and on its own that made every such sentence
/// commented-out code: too short for [`is_prose_sentence`]'s word threshold and
/// without the terminal full stop its shorter rule needs. The distinguishing
/// feature is the company the path keeps — real code surrounds a path with
/// brackets, operators or a leading keyword, whereas prose surrounds it with
/// words. So: every token must read as a word (markdown backticks trimmed), at
/// least [`PROSE_PATH_MIN_PLAIN_WORDS`] of them must be path-free, and the line
/// must not open with a code keyword (which keeps `use std::io::Error;` code).
fn is_prose_quoting_a_path(line: &str) -> bool {
    let mut plain = 0usize;
    let mut paths = 0usize;
    for token in line.split_whitespace() {
        let token = token.trim_matches('`');
        if token.is_empty() {
            continue;
        }
        if !is_wordish(token) {
            return false;
        }
        if token.contains("::") {
            paths = paths.saturating_add(1);
        } else {
            plain = plain.saturating_add(1);
        }
    }
    paths > 0 && plain >= PROSE_PATH_MIN_PLAIN_WORDS && !first_token_is_keyword(line)
}

/// Whether a whitespace-delimited token reads as a natural-language word:
/// letters plus the punctuation that legitimately attaches to one. Notably
/// excludes `(`, `)`, `=`, `{`, `}` and `_`, which are the shapes that make a
/// token look like source rather than English.
fn is_wordish(word: &str) -> bool {
    let mut chars = word.chars().peekable();
    if chars.peek().is_none() {
        return false;
    }
    word.chars()
        .all(|c| c.is_ascii_alphabetic() || matches!(c, '.' | ',' | '\'' | '-' | '!' | '?' | ';' | ':'))
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

    /// Policy reversal from the "any line is code" rule: a block that mixes
    /// prose and code is kept whole. Flagging it meant `--fix` deleted the code
    /// line out of the middle of the paragraph, which is the data loss this
    /// backend was reported for.
    #[test]
    fn rejects_a_block_that_mixes_prose_and_code() {
        let block = "# increment the counter before returning\n# counter += 1;";
        assert!(!looks_like_commented_out_code(block));
    }

    #[test]
    fn flags_a_block_that_is_code_on_every_line() {
        let block = "// let x = compute(a);\n//\n// println!(\"{x}\");";
        assert!(
            looks_like_commented_out_code(block),
            "a blank comment line is neutral, not a veto"
        );
    }

    #[test]
    fn rejects_prose_quoting_a_path() {
        assert!(!looks_like_commented_out_code("// returns Value::String here"));
        assert!(!looks_like_commented_out_code("// returns `Value::String` here"));
        assert!(!looks_like_commented_out_code("// we return std::io::Error on failure"));
        assert!(!looks_like_commented_out_code("// see HashMap::new for details"));
    }

    #[test]
    fn flags_bare_paths_and_statements() {
        assert!(looks_like_commented_out_code("// a::b"));
        assert!(looks_like_commented_out_code("// foo(bar); baz"));
        assert!(looks_like_commented_out_code("// use std::io::Error;"));
    }
}
