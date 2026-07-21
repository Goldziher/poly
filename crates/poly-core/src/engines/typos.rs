//! Cross-cutting spell-checker backend: wraps the `typos` + `typos-dict` crates.
//!
//! Declared for zero languages (`languages() = &[]`). The registry appends
//! this engine to **every** language match arm so all files are spell-checked
//! in addition to their own formatter/linter.
//!
//! # Capabilities
//!
//! Lint only, and **never autofix**. Misspellings are reported at
//! [`Severity::Warning`] with the dictionary suggestion in the message, but carry
//! no [`Edit`](crate::engine::Edit) — auto-correcting a typo silently rewrites identifiers, string
//! keys, and API names that only *look* like misspellings, which regresses code
//! far more often than it helps. A typo must be resolved by a human.
//!
//! # Version string
//!
//! Encodes both the `typos` tokeniser version and the `typos-dict` word-list
//! version because either changing would alter output and must bust the cache.
//!
//! # Dictionary
//!
//! Uses `typos_dict::WORD` directly. Locale variants (`typos-vars`) are
//! intentionally excluded — they classify valid spellings as typos depending
//! on locale preference, which is inappropriate for a general-purpose backend.

use std::borrow::Cow;

use globset::{Glob, GlobSetBuilder};
use unicase::UniCase;

use crate::config::EngineConfig;
use crate::engine::{Capabilities, Diagnostic, Engine, Severity, SourceFile, Span};
use crate::language::Language;

/// Combined cache-key version: `typos` tokeniser + `typos-dict` word list,
/// plus a marker for the noise-suppression guards below. Bump whenever either
/// crate is updated OR the guard logic changes (it alters output).
const TYPOS_VERSION: &str = "0.10.43+dict-0.13.31+guards1+cfg2+warn-noautofix+builtins1";

/// Skip spell-checking files at least this large: generated/minified bundles
/// dominate by size and are pure noise word-by-word.
const MAX_SPELL_CHECK_BYTES: usize = 1 << 20;

/// Skip files containing any line at least this long: very long lines are a
/// reliable signal of minified/generated content (one 11.7 MB Plotly bundle in
/// the dry-run corpus produced ~5k false positives each), not hand-written
/// prose or code.
const MAX_LINE_BYTES: usize = 2_000;

/// Minimum length of a flagged token. Ultra-short corrections (two-letter
/// minified identifiers) are overwhelmingly noise rather than real typos, so
/// require at least this many bytes; common three-letter typos are kept.
const MIN_TYPO_LEN: usize = 3;

/// Words the built-in dictionary flags but that are correct in essentially any
/// codebase: established technical abbreviations and well-known OSS
/// tool/library/format names. Baking them in spares every repo from re-listing
/// them in `[lint.*.typos] extend_words` (they recur across the dry-run corpus).
/// Deliberately narrow — only tokens that are proper nouns (never a real
/// misspelling) or firmly established terms; project-specific jargon stays in the
/// per-repo config so it can't mask genuine typos elsewhere. All entries are
/// lowercase and compared case-insensitively.
static BUILTIN_VALID_WORDS: &[&str] = &[
    "ser",
    "flate",
    "fpr",
    "arange",
    "unparseable",
    "certifi",
    "onnx",
    "wasm",
    "tesseract",
    "pdfium",
    "pymupdf",
    "surrealdb",
    "mkdocs",
    "mkdocstrings",
    "rumdl",
];

/// Whether `lowercased` is a built-in always-valid word (see [`BUILTIN_VALID_WORDS`]).
fn is_builtin_valid_word(lowercased: &str) -> bool {
    BUILTIN_VALID_WORDS.contains(&lowercased)
}

/// Cross-cutting spell-checker declares no tier-1 language ownership.
static LANGUAGES: &[Language] = &[];

/// Shared tokeniser — `Tokenizer::new()` is `const fn`, so this is safe.
static TOKENIZER: typos::tokens::Tokenizer = typos::tokens::Tokenizer::new();

/// Cross-cutting spell-checker backed by the published `typos`/`typos-dict` crates.
pub struct TyposEngine;

impl Engine for TyposEngine {
    fn name(&self) -> &'static str {
        "typos"
    }

    fn languages(&self) -> &'static [Language] {
        LANGUAGES
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            lint: true,
            format: false,
            fix: false,
        }
    }

    fn version(&self) -> &str {
        TYPOS_VERSION
    }

    fn lint(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        if is_generated_or_minified(&src.content) {
            return Ok(Vec::new());
        }

        let exclude_globs: Vec<String> = cfg
            .options
            .get("extend_exclude")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).map(|s| s.to_string()).collect())
            .unwrap_or_default();
        if !exclude_globs.is_empty() && path_matches_any(&src.path, &exclude_globs) {
            return Ok(Vec::new());
        }

        let mut valid_words: Vec<String> = cfg
            .options
            .get("extend_ignore_words")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.to_ascii_lowercase())
                    .collect()
            })
            .unwrap_or_default();
        if let Some(words_table) = cfg.options.get("extend_words").and_then(|v| v.as_table()) {
            valid_words.extend(words_table.keys().map(|k| k.to_ascii_lowercase()));
        }

        let valid_identifiers: Vec<String> = cfg
            .options
            .get("extend_identifiers")
            .and_then(|v| v.as_table())
            .map(|t| t.keys().map(|k| k.to_ascii_lowercase()).collect())
            .unwrap_or_default();

        let dict = ConfiguredDictionary {
            valid_words: &valid_words,
            valid_identifiers: &valid_identifiers,
        };

        let ignore_re = compile_regexes(&str_array(&cfg.options, "extend_ignore_re"));
        let word_ignore_re = compile_regexes(&str_array(&cfg.options, "extend_ignore_words_re"));
        let ident_ignore_re = compile_regexes(&str_array(&cfg.options, "extend_ignore_identifiers_re"));
        let masked: Vec<(usize, usize)> = ignore_re
            .iter()
            .flat_map(|re| re.find_iter(&src.content).map(|m| (m.start(), m.end())))
            .collect();

        let diags = typos::check_str(&src.content, &TOKENIZER, &dict)
            .filter(|typo| typo.typo.len() >= MIN_TYPO_LEN)
            .filter(|typo| !byte_in_ranges(typo.byte_offset, &masked))
            .filter(|typo| !word_ignore_re.iter().any(|re| re.is_match(typo.typo.as_ref())))
            .filter(|typo| !ident_ignore_re.iter().any(|re| re.is_match(typo.typo.as_ref())))
            .map(|typo| typo_to_diagnostic(&src.content, typo))
            .collect();
        Ok(diags)
    }
}

/// Read `options[key]` as an array of strings (empty when absent or wrong type).
fn str_array(options: &toml::Table, key: &str) -> Vec<String> {
    options
        .get(key)
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).map(str::to_string).collect())
        .unwrap_or_default()
}

/// Compile `patterns` into regexes, skipping (with a warning) any that fail to
/// compile so one malformed user pattern never aborts the whole run.
fn compile_regexes(patterns: &[String]) -> Vec<regex::Regex> {
    patterns
        .iter()
        .filter_map(|p| match regex::Regex::new(p) {
            Ok(re) => Some(re),
            Err(_) => {
                tracing::warn!(pattern = %p, engine = "typos", "invalid extend-ignore regex; skipping");
                None
            }
        })
        .collect()
}

/// Whether `offset` falls within any `[start, end)` range.
fn byte_in_ranges(offset: usize, ranges: &[(usize, usize)]) -> bool {
    ranges.iter().any(|&(start, end)| offset >= start && offset < end)
}

/// Whether `content` looks generated/minified and should not be spell-checked:
/// at least [`MAX_SPELL_CHECK_BYTES`] in size, or containing any line of at
/// least [`MAX_LINE_BYTES`]. Both are reliable signals of machine-emitted
/// bundles rather than hand-written prose or code.
fn is_generated_or_minified(content: &str) -> bool {
    content.len() >= MAX_SPELL_CHECK_BYTES || content.lines().any(|line| line.len() >= MAX_LINE_BYTES)
}

/// Return `true` when `path` matches at least one of the gitignore-style glob
/// `patterns`. Matching is tried against the full path first, then against each
/// successive suffix (leading component stripped repeatedly) so bare patterns
/// like `CHANGELOG.md` match `/repo/CHANGELOG.md` and `tests/**` matches
/// `/repo/tests/foo.rs`. Malformed patterns are silently ignored.
fn path_matches_any(path: &std::path::Path, patterns: &[String]) -> bool {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        if let Ok(glob) = Glob::new(pattern) {
            builder.add(glob);
        }
    }
    let Ok(set) = builder.build() else {
        return false;
    };

    if set.is_match(path) {
        return true;
    }
    let mut tail: &std::path::Path = path;
    loop {
        let mut comps = tail.components();
        let Some(_) = comps.next() else { break };
        let rest = comps.as_path();
        if rest == tail || rest.as_os_str().is_empty() {
            break;
        }
        if set.is_match(rest) {
            return true;
        }
        tail = rest;
    }
    false
}

/// In-process dictionary wrapping `typos_dict::WORD`, extended with a
/// caller-supplied list of words to treat as valid.
///
/// `valid_words` contains **lowercased** word strings. Any token whose lowercased
/// form appears in that slice is returned as [`typos::Status::Valid`], bypassing
/// the built-in dictionary lookup.
///
/// `valid_identifiers` contains **lowercased** identifier strings. Any identifier
/// token whose lowercased form appears in that slice is returned as valid,
/// suppressing any word-level typo flagging within it.
///
/// Implements [`typos::Dictionary`] so it can be passed to [`typos::check_str`].
struct ConfiguredDictionary<'a> {
    /// Lowercased words the user wants treated as valid spellings.
    /// Sourced from `extend_ignore_words` (flat list) and `extend_words` keys.
    valid_words: &'a [String],
    /// Lowercased identifier tokens the user wants treated as valid.
    /// Sourced from `extend_identifiers` keys.
    valid_identifiers: &'a [String],
}

impl typos::Dictionary for ConfiguredDictionary<'_> {
    fn correct_ident<'s>(&'s self, ident: typos::tokens::Identifier<'_>) -> Option<typos::Status<'s>> {
        match ident.token() {
            "O_WRONLY" | "dBA" => return Some(typos::Status::Valid),
            _ => {}
        }
        let lowered = ident.token().to_ascii_lowercase();
        if self.valid_identifiers.iter().any(|w| w == &lowered) {
            return Some(typos::Status::Valid);
        }
        None
    }

    fn correct_word<'s>(&'s self, word_token: typos::tokens::Word<'_>) -> Option<typos::Status<'s>> {
        use typos::tokens::Case;

        if word_token.case() == Case::None {
            return None;
        }

        let lowered = word_token.token().to_ascii_lowercase();
        if is_builtin_valid_word(&lowered) || self.valid_words.iter().any(|w| w == &lowered) {
            return Some(typos::Status::Valid);
        }

        let word_case = UniCase::new(word_token.token());
        let corrections = typos_dict::WORD.find(&word_case).copied()?;

        let mut status = if corrections.is_empty() {
            typos::Status::Invalid
        } else {
            typos::Status::Corrections(corrections.iter().map(|c| Cow::Borrowed(*c)).collect())
        };

        for s in status.corrections_mut() {
            case_correct(s, word_token.case());
        }

        Some(status)
    }
}

/// Adjust `correction` to match `case` (mirrors typos-cli's `case_correct`).
fn case_correct(correction: &mut Cow<'_, str>, case: typos::tokens::Case) {
    use typos::tokens::Case;
    match case {
        Case::Lower | Case::None => {}
        Case::Title => {
            let s = correction.to_mut();
            if !s.is_empty() {
                // SAFETY: ASCII-only index 0..1 on a non-empty ASCII string.
                s[0..1].make_ascii_uppercase();
            }
        }
        Case::Upper => {
            correction.to_mut().make_ascii_uppercase();
        }
    }
}

fn typo_to_diagnostic(content: &str, typo: typos::Typo<'_>) -> Diagnostic {
    let start_byte = typo.byte_offset;
    let end_byte = start_byte + typo.typo.len();

    let (start_line, start_col) = byte_to_line_col(content, start_byte);
    let (end_line, end_col) = byte_to_line_col(content, end_byte);

    let corrections: Vec<&str> = match &typo.corrections {
        typos::Status::Corrections(c) => c.iter().map(Cow::as_ref).collect(),
        _ => vec![],
    };

    let message = if corrections.is_empty() {
        format!("`{}` is a misspelling", typo.typo)
    } else {
        format!("`{}` should be `{}`", typo.typo, corrections.join("` or `"))
    };

    Diagnostic {
        engine: "typos".to_string(),
        code: Some("typo".to_string()),
        // Warning, not Error: a single dictionary false positive (a real word
        // inside an identifier, a short hex/base64 fragment, a domain term not in
        // the word list) must not fail CI. `poly lint` exits non-zero only on
        // Error severity, so spell-check findings surface without breaking builds.
        // Users who want hard failures can raise the severity in config.
        severity: Severity::Warning,
        title: message,
        description: None,
        url: None,
        span: Some(Span {
            start_line,
            start_col,
            end_line,
            end_col,
        }),
        fix: Vec::new(),
        metadata: Default::default(),
    }
}

/// Convert a byte offset to a 1-based (line, column) pair.
///
/// Columns count UTF-8 bytes from the start of the line, not Unicode scalar
/// values — this matches the convention used by most editors.
fn byte_to_line_col(content: &str, offset: usize) -> (u32, u32) {
    let clamped = offset.min(content.len());
    let before = &content[..clamped];
    let line = (before.bytes().filter(|&b| b == b'\n').count() as u32) + 1;
    let col_start = before.rfind('\n').map_or(0, |p| p + 1);
    let col = (clamped - col_start) as u32 + 1;
    (line, col)
}
