//! Ruby backend: formatting via [`rubyfmt`].
//!
//! rubyfmt is a fully opinionated Ruby formatter built on the pure-Rust
//! Prism parser (vendored C library, no system Ruby required). poly
//! respects its output as-is — the same way `gofmt` is authoritative for Go.
//! Format-only: no lint, no fix.
//!
//! # Build requirements
//! rubyfmt depends on `ruby-prism-sys` which compiles a vendored C library
//! (`libprism.a`) using `cc` and generates Rust bindings via `bindgen`.
//! `bindgen` requires `libclang` at build time (provided by the system
//! Clang/LLVM toolchain); runtime has no system dependency.

use rubyfmt::{RichFormatError, format_buffer};

use crate::config::EngineConfig;
use crate::engine::{Capabilities, Diagnostic, Engine, FormatOutput, SourceFile};
use crate::language::Language;

/// rubyfmt Ruby backend — format-only for `.rb` files.
pub struct RubyfmtEngine;

/// rubyfmt pinned git rev; folded into the cache key so a rev bump invalidates
/// any stale cached output. rubyfmt is a git dependency (no meaningful crates.io
/// version), so the short rev is the authoritative source identifier.
const VERSION: &str = "rubyfmt-git:d3d433c";

/// Languages handled by this backend.
static LANGUAGES: &[Language] = &[Language::Ruby];

impl Engine for RubyfmtEngine {
    fn name(&self) -> &'static str {
        "rubyfmt"
    }

    fn languages(&self) -> &'static [Language] {
        LANGUAGES
    }

    /// Format-only: rubyfmt neither reports diagnostics nor applies fixes.
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            lint: false,
            format: true,
            fix: false,
        }
    }

    fn version(&self) -> &str {
        VERSION
    }

    /// No-op: rubyfmt has no lint capability.
    fn lint(&self, _src: &SourceFile, _cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        Ok(vec![])
    }

    /// Format `src.content` with rubyfmt. Returns [`FormatOutput::Unchanged`]
    /// when the output equals the input (already formatted) and when the input
    /// cannot be parsed, so unparsable Ruby is never corrupted.
    ///
    /// Config is intentionally unused: rubyfmt is a zero-configuration
    /// formatter (no line-length or indent knobs). This mirrors `gofmt` —
    /// the tool's output is authoritative.
    fn format(&self, src: &SourceFile, _cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
        match format_buffer(src.content.as_bytes()) {
            Ok(bytes) => {
                // rubyfmt always emits valid UTF-8 Ruby source. If the bytes
                // are somehow not valid UTF-8, leave the file untouched rather
                // than corrupting it.
                match String::from_utf8(bytes) {
                    Ok(formatted) => {
                        if formatted == *src.content {
                            Ok(FormatOutput::Unchanged)
                        } else {
                            Ok(FormatOutput::Formatted(formatted))
                        }
                    }
                    Err(_) => Ok(FormatOutput::Unchanged),
                }
            }
            // SyntaxError: rubyfmt could not parse the file. Leave untouched.
            // IOError: internal I/O failure. Leave untouched.
            // Robustness rule: every file in the corpus must survive a format
            // run — never propagate a format error as a pipeline error.
            Err(RichFormatError::SyntaxError | RichFormatError::IOError(_)) => Ok(FormatOutput::Unchanged),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::config::GlobalDefaults;

    fn make_src(content: &str) -> SourceFile {
        SourceFile {
            path: PathBuf::from("test.rb"),
            language: Language::Ruby,
            content: content.into(),
        }
    }

    fn default_cfg() -> EngineConfig {
        EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 2,
            options: toml::Table::new(),
        }
    }

    #[test]
    fn engine_metadata() {
        let engine = RubyfmtEngine;
        assert_eq!(engine.name(), "rubyfmt");
        assert_eq!(engine.languages(), &[Language::Ruby]);
        let caps = engine.capabilities();
        assert!(!caps.lint, "rubyfmt has no lint capability");
        assert!(caps.format, "rubyfmt must report format capability");
        assert!(!caps.fix, "rubyfmt has no fix capability");
        assert_eq!(engine.version(), VERSION);
    }

    #[test]
    fn lint_always_returns_empty() {
        let engine = RubyfmtEngine;
        let src = make_src("def hello; end\n");
        let diags = engine.lint(&src, &default_cfg()).unwrap();
        assert!(diags.is_empty());
    }

    #[test]
    fn should_reformat_block_with_leading_blank_line() {
        // rubyfmt removes blank lines at the start of blocks: `a do\n\nend`
        // becomes `a do\nend`.
        let engine = RubyfmtEngine;
        let src = make_src("a do\n\nend\n");
        let out = engine.format(&src, &default_cfg()).unwrap();
        assert!(
            matches!(out, FormatOutput::Formatted(_)),
            "expected Formatted for input with spurious blank line in block"
        );
    }

    #[test]
    fn should_return_unchanged_on_unparsable_ruby() {
        // An unclosed `def` is a syntax error — rubyfmt returns SyntaxError,
        // which we map to Unchanged (robustness rule).
        let engine = RubyfmtEngine;
        let src = make_src("def foo(");
        let out = engine.format(&src, &default_cfg()).unwrap();
        assert!(
            matches!(out, FormatOutput::Unchanged),
            "expected Unchanged for syntax-error input, got Formatted"
        );
    }
}
