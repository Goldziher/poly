//! Reference backend: language-agnostic whitespace normalization.
//!
//! This is the template every real backend follows, and the catch-all that
//! serves any language until a native or tree-sitter backend is wired in. It
//! trims trailing whitespace, normalizes line endings, and enforces a final
//! newline ([`crate::defaults::normalize_whitespace`]).

use crate::config::EngineConfig;
use crate::defaults::normalize_whitespace;
use crate::engine::{Capabilities, Diagnostic, Engine, FormatOutput, Severity, SourceFile, Span};
use crate::language::Language;

/// Language-agnostic whitespace-normalization backend (see module docs).
pub struct WhitespaceEngine;

/// The generic backend declares no tier-1 languages; the registry routes to it.
static LANGUAGES: &[Language] = &[];

impl Engine for WhitespaceEngine {
    fn name(&self) -> &'static str {
        "whitespace"
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

    fn format(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
        let formatted = normalize_whitespace(&src.content, &cfg.globals);
        if formatted == src.content {
            Ok(FormatOutput::Unchanged)
        } else {
            Ok(FormatOutput::Formatted(formatted))
        }
    }

    fn lint(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        if !cfg.globals.trim_trailing_whitespace {
            return Ok(Vec::new());
        }
        let mut diags = Vec::new();
        for (i, raw) in src.content.split('\n').enumerate() {
            let line = raw.strip_suffix('\r').unwrap_or(raw);
            let trimmed_len = line.trim_end().len();
            if trimmed_len != line.len() {
                diags.push(Diagnostic {
                    engine: "whitespace".to_string(),
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
                });
            }
        }
        Ok(diags)
    }
}
