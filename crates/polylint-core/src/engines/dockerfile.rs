//! Dockerfile linter backend — hadolint-style rules via [`dockerfile_parser`].
//!
//! Lint-only (no formatter exists): [`format`][Engine::format] returns
//! [`FormatOutput::Unchanged`] so the runner skips the write path.
//! All rule codes use hadolint's canonical `DLxxxx` scheme for familiarity.
//!
//! # Rules implemented
//!
//! | Code   | Severity | What it checks                                              |
//! |--------|----------|-------------------------------------------------------------|
//! | DL3000 | error    | `WORKDIR` with a relative path                              |
//! | DL3006 | warning  | `FROM` without an explicit image tag or digest              |
//! | DL3007 | warning  | `FROM` using the implicit `:latest` tag                     |
//! | DL3009 | warning  | `apt-get install` without apt-get cache cleanup             |
//! | DL3025 | warning  | `CMD` / `ENTRYPOINT` in shell form (use JSON array instead) |
//! | DL3059 | warning  | Multiple consecutive `RUN` instructions                     |
//! | DL4000 | warning  | `MAINTAINER` instruction is deprecated                      |
//! | DL4001 | warning  | Both `wget` and `curl` used in `RUN` commands               |

use dockerfile_parser::{Dockerfile, Instruction};

use crate::config::EngineConfig;
use crate::engine::{
    Capabilities, Diagnostic, Engine, FormatOutput, Severity, SourceFile, Span as EngineSpan,
};
use crate::language::Language;

// ---------------------------------------------------------------------------
// Rule codes — named constants so there are no magic strings in rule bodies.
// ---------------------------------------------------------------------------

/// DL3000: Use absolute path for WORKDIR.
const DL3000: &str = "DL3000";
/// DL3006: Always tag the version of the image you use.
const DL3006: &str = "DL3006";
/// DL3007: Using `:latest` is prone to errors if the image ever updates.
const DL3007: &str = "DL3007";
/// DL3009: Delete apt-get lists after installing packages.
const DL3009: &str = "DL3009";
/// DL3025: Use JSON array notation for CMD and ENTRYPOINT.
const DL3025: &str = "DL3025";
/// DL3059: Multiple consecutive `RUN` instructions; consolidate them.
const DL3059: &str = "DL3059";
/// DL4000: MAINTAINER is deprecated; use LABEL instead.
const DL4000: &str = "DL4000";
/// DL4001: Use either wget or curl, not both.
const DL4001: &str = "DL4001";

// ---------------------------------------------------------------------------
// Cache-key version: the dockerfile-parser crate version.  Bump this string
// whenever the parser or any rule logic changes so stale cache entries are
// invalidated.
// ---------------------------------------------------------------------------

/// dockerfile-parser crate version embedded into the cache key.
const DOCKERFILE_PARSER_VERSION: &str = "0.9.0+parse-diag-v1";

/// Diagnostic code emitted when the Dockerfile cannot be parsed at all.
const PARSE_ERROR: &str = "parse-error";

/// Languages handled by this backend.
static LANGUAGES: &[Language] = &[Language::Dockerfile];

/// Dockerfile linter backend.
pub struct DockerfileEngine;

impl Engine for DockerfileEngine {
    fn name(&self) -> &'static str {
        "dockerfile"
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
        DOCKERFILE_PARSER_VERSION
    }

    fn lint(&self, src: &SourceFile, _cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        let dockerfile = match Dockerfile::parse(&src.content) {
            Ok(df) => df,
            Err(e) => {
                return Ok(vec![make_diag(
                    self.name(),
                    PARSE_ERROR,
                    Severity::Error,
                    format!("parse error: {e}"),
                    None,
                )]);
            }
        };

        let mut diags: Vec<Diagnostic> = Vec::new();

        // DL4001 state: track where wget/curl first appear across all RUN commands.
        let mut wget_span: Option<EngineSpan> = None;
        let mut curl_span: Option<EngineSpan> = None;

        for (idx, ins) in dockerfile.instructions.iter().enumerate() {
            match ins {
                Instruction::From(from) => {
                    check_from(self.name(), from, &dockerfile, &mut diags);
                }

                Instruction::Run(run) => {
                    // DL3059: consecutive RUN instructions.
                    if idx > 0 && matches!(dockerfile.instructions[idx - 1], Instruction::Run(_)) {
                        diags.push(make_diag(
                            self.name(),
                            DL3059,
                            Severity::Warning,
                            "Multiple consecutive `RUN` instructions; \
                             consolidate into one using `&&`.",
                            Some(span_of(run.span, &dockerfile)),
                        ));
                    }

                    // Shell-form checks (DL3009, DL4001).
                    if let Some(shell) = run.as_shell() {
                        let text = shell.to_string();

                        // DL3009: apt-get install without cache cleanup.
                        if text.contains("apt-get install")
                            && !text.contains("rm -rf /var/lib/apt/lists")
                        {
                            diags.push(make_diag(
                                self.name(),
                                DL3009,
                                Severity::Warning,
                                "Delete the apt-get cache after installation: \
                                 `&& rm -rf /var/lib/apt/lists/*`.",
                                Some(span_of(run.span, &dockerfile)),
                            ));
                        }

                        // DL4001 accumulator: record first wget / curl sighting.
                        if wget_span.is_none() && contains_tool(&text, "wget") {
                            wget_span = Some(span_of(run.span, &dockerfile));
                        }
                        if curl_span.is_none() && contains_tool(&text, "curl") {
                            curl_span = Some(span_of(run.span, &dockerfile));
                        }
                    }
                }

                Instruction::Cmd(cmd) => {
                    // DL3025: CMD in shell form.
                    if cmd.as_shell().is_some() {
                        diags.push(make_diag(
                            self.name(),
                            DL3025,
                            Severity::Warning,
                            "Use JSON exec notation for CMD: \
                             `CMD [\"executable\", \"arg1\"]`.",
                            Some(span_of(cmd.span, &dockerfile)),
                        ));
                    }
                }

                Instruction::Entrypoint(ep) => {
                    // DL3025: ENTRYPOINT in shell form.
                    if ep.as_shell().is_some() {
                        diags.push(make_diag(
                            self.name(),
                            DL3025,
                            Severity::Warning,
                            "Use JSON exec notation for ENTRYPOINT: \
                             `ENTRYPOINT [\"executable\", \"arg1\"]`.",
                            Some(span_of(ep.span, &dockerfile)),
                        ));
                    }
                }

                Instruction::Misc(misc) => {
                    check_misc(self.name(), misc, &dockerfile, &mut diags);
                }

                // ARG, LABEL, COPY, ENV — no rules yet.
                _ => {}
            }
        }

        // DL4001: emit once if both wget and curl are present in any RUN.
        if wget_span.is_some() && curl_span.is_some() {
            // Point to the curl occurrence; hadolint recommends preferring wget.
            diags.push(make_diag(
                self.name(),
                DL4001,
                Severity::Warning,
                "Use either `wget` or `curl` to fetch files, not both. Prefer `wget`.",
                curl_span,
            ));
        }

        Ok(diags)
    }

    fn format(&self, _src: &SourceFile, _cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
        Ok(FormatOutput::Unchanged)
    }
}

// ---------------------------------------------------------------------------
// Per-instruction rule helpers — extracted to keep `lint` readable and each
// rule within the 20-complexity ceiling.
// ---------------------------------------------------------------------------

/// FROM instruction checks: DL3006 (no tag/digest) and DL3007 (`:latest`).
fn check_from(
    engine: &str,
    from: &dockerfile_parser::FromInstruction,
    dockerfile: &Dockerfile,
    diags: &mut Vec<Diagnostic>,
) {
    let image = &from.image_parsed;

    // DL3006: no tag and no digest.  Skip variable-reference images (e.g.
    // `FROM $BASE_IMAGE`) because we cannot statically know the tag.
    if image.tag.is_none() && image.hash.is_none() && !image.image.starts_with('$') {
        diags.push(make_diag(
            engine,
            DL3006,
            Severity::Warning,
            format!(
                "Image `{}` has no explicit tag or digest; \
                 pin a version to avoid accidental `:latest` pulls.",
                image.image
            ),
            Some(span_of(from.span, dockerfile)),
        ));
    }

    // DL3007: explicit `:latest` tag.
    if image.tag.as_deref() == Some("latest") {
        diags.push(make_diag(
            engine,
            DL3007,
            Severity::Warning,
            format!(
                "Using `:latest` for `{}` is error-prone; pin a specific version.",
                image.image
            ),
            Some(span_of(from.span, dockerfile)),
        ));
    }
}

/// MISC instruction checks: DL4000 (MAINTAINER) and DL3000 (relative WORKDIR).
fn check_misc(
    engine: &str,
    misc: &dockerfile_parser::MiscInstruction,
    dockerfile: &Dockerfile,
    diags: &mut Vec<Diagnostic>,
) {
    // Use the raw instruction name from the AST; uppercase for case-insensitive matching.
    match misc.instruction.content.to_ascii_uppercase().as_str() {
        "MAINTAINER" => {
            diags.push(make_diag(
                engine,
                DL4000,
                Severity::Warning,
                "`MAINTAINER` is deprecated; replace with `LABEL maintainer=\"...\"` instead.",
                Some(span_of(misc.span, dockerfile)),
            ));
        }

        "WORKDIR" => {
            // DL3000: WORKDIR with a relative path.
            // Arguments may contain leading whitespace from the parser.
            let arg = misc.arguments.to_string();
            let trimmed = arg.trim();

            // A path is "absolute" if it starts with `/` (Unix) or `$` (variable
            // expansion — cannot be statically classified, so we skip it).
            if !trimmed.is_empty() && !trimmed.starts_with('/') && !trimmed.starts_with('$') {
                diags.push(make_diag(
                    engine,
                    DL3000,
                    Severity::Error,
                    format!(
                        "WORKDIR path `{trimmed}` is relative; use an absolute path \
                         (starting with `/`)."
                    ),
                    Some(span_of(misc.span, dockerfile)),
                ));
            }
        }

        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Span conversion
// ---------------------------------------------------------------------------

/// Convert a [`dockerfile_parser::Span`] (byte offsets into the file) to the
/// engine's 1-based line/column [`EngineSpan`].
///
/// `relative_span` walks the source bytes to find the line boundary, returning
/// a 0-indexed line number and a line-relative byte-offset span.  We add 1 to
/// both for the 1-based convention polylint uses throughout.
fn span_of(df_span: dockerfile_parser::Span, dockerfile: &Dockerfile) -> EngineSpan {
    let (line_0, col_span) = df_span.relative_span(dockerfile);
    EngineSpan {
        start_line: (line_0 as u32).saturating_add(1),
        start_col: (col_span.start as u32).saturating_add(1),
        end_line: (line_0 as u32).saturating_add(1),
        end_col: (col_span.end as u32).saturating_add(1),
    }
}

// ---------------------------------------------------------------------------
// Diagnostic builder
// ---------------------------------------------------------------------------

/// Build a normalized [`Diagnostic`].
fn make_diag(
    engine: &str,
    code: &'static str,
    severity: Severity,
    message: impl Into<String>,
    span: Option<EngineSpan>,
) -> Diagnostic {
    Diagnostic {
        engine: engine.to_owned(),
        code: Some(code.to_owned()),
        severity,
        title: message.into(),
        description: None,
        span,
        url: None,
        fix: Vec::new(),
        metadata: std::collections::BTreeMap::new(),
    }
}

// ---------------------------------------------------------------------------
// Utility helpers
// ---------------------------------------------------------------------------

/// Returns `true` when `text` contains `tool` as a standalone token.
///
/// A simple `str::contains` would match `curlftpfs` for `curl`.  This helper
/// requires the tool name to be preceded and followed by a non-word character
/// (space, `/`, `\t`, `&`, `|`, `;`, start/end of string) so false positives
/// are avoided.
fn contains_tool(text: &str, tool: &str) -> bool {
    let bytes = text.as_bytes();
    let tool_len = tool.len();
    let mut start = 0;

    while let Some(pos) = text[start..].find(tool) {
        let abs = start + pos;
        let before_ok = abs == 0 || !bytes[abs - 1].is_ascii_alphanumeric();
        let after_ok =
            (abs + tool_len) >= bytes.len() || !bytes[abs + tool_len].is_ascii_alphanumeric();

        if before_ok && after_ok {
            return true;
        }
        start = abs + 1;
    }
    false
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use crate::config::{EngineConfig, GlobalDefaults};

    use super::*;

    fn engine_cfg() -> EngineConfig {
        EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 4,
            options: toml::Table::new(),
        }
    }

    #[test]
    fn contains_tool_avoids_prefix_match() {
        // "curlftpfs" must NOT match "curl"
        assert!(!contains_tool("RUN apt-get install curlftpfs", "curl"));
        // standalone "curl" must match
        assert!(contains_tool("RUN curl https://example.com", "curl"));
        assert!(contains_tool("RUN apt-get install curl wget", "wget"));
    }

    #[test]
    fn relative_workdir_fires() {
        let engine = DockerfileEngine;
        let src = SourceFile {
            path: "Dockerfile".into(),
            language: Language::Dockerfile,
            content: "FROM alpine:3.18\nWORKDIR app\n".into(),
        };
        let diags = engine.lint(&src, &engine_cfg()).unwrap();
        assert!(
            diags.iter().any(|d| d.code.as_deref() == Some(DL3000)),
            "expected DL3000 for relative WORKDIR"
        );
    }

    #[test]
    fn absolute_workdir_ok() {
        let engine = DockerfileEngine;
        let src = SourceFile {
            path: "Dockerfile".into(),
            language: Language::Dockerfile,
            content: "FROM alpine:3.18\nWORKDIR /app\n".into(),
        };
        let diags = engine.lint(&src, &engine_cfg()).unwrap();
        assert!(
            !diags.iter().any(|d| d.code.as_deref() == Some(DL3000)),
            "should not fire DL3000 for absolute WORKDIR"
        );
    }

    #[test]
    fn parse_failure_produces_error_diagnostic() {
        let engine = DockerfileEngine;
        // An ambiguous line continuation after a complete LABEL value is a
        // documented parse error in dockerfile-parser (the backslash after the
        // closing `"` makes the continuation ambiguous).
        let src = SourceFile {
            path: "Dockerfile".into(),
            language: Language::Dockerfile,
            content: "LABEL foo=\"bar\\\n      baz\"\\\nRUN foo\n".into(),
        };
        let diags = engine.lint(&src, &engine_cfg()).unwrap();
        assert!(
            !diags.is_empty(),
            "a malformed Dockerfile must produce at least one diagnostic"
        );
        let parse_diag = diags
            .iter()
            .find(|d| d.code.as_deref() == Some(PARSE_ERROR));
        assert!(
            parse_diag.is_some(),
            "expected a parse-error diagnostic, got: {diags:?}"
        );
        assert_eq!(
            parse_diag.unwrap().severity,
            Severity::Error,
            "parse-error diagnostic must have Error severity"
        );
    }
}
