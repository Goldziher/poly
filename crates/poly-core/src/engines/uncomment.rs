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
const UNCOMMENT_VERSION: &str = "uncomment-3.4.0+map1";

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
    cfg.options
        .get("enabled")
        .and_then(toml::Value::as_bool)
        .unwrap_or(false)
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
