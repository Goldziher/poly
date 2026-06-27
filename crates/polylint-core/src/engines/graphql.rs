//! GraphQL backend: formatting and parse-error lint via `graphql-parser`.
//!
//! Capabilities: [`Capabilities::lint`] (parse-error diagnostics) and
//! [`Capabilities::format`] (canonical pretty-print via `Document::format`).
//!
//! Both GraphQL Schema Definition Language (SDL) files and query/operation
//! files share the `.graphql` / `.gql` extension. The backend tries SDL
//! parsing first (most common in project repositories), then falls back to
//! query document parsing. If both fail, the parse error is surfaced as a
//! [`Diagnostic`].
//!
//! # Opinionated defaults
//!
//! | Setting | Polylint default | graphql-parser default |
//! |---|---|---|
//! | `indent` | 2 | 2 |
//!
//! `graphql-parser` does not expose a line-length setting; its formatter
//! emits one field / argument per line, so line length is not a meaningful
//! concept. The indent width is taken from [`EngineConfig::indent_width`]
//! (itself derived from [`Language::default_indent_width`], which is 2 for
//! GraphQL) and can be overridden via `[fmt.graphql.graphql]` in
//! `polylint.toml`.

use graphql_parser::Style;
use graphql_parser::query::parse_query;
use graphql_parser::schema::parse_schema;

use crate::config::EngineConfig;
use crate::engine::{Capabilities, Diagnostic, Engine, FormatOutput, Severity, SourceFile, Span};
use crate::language::Language;

/// GraphQL backend powered by `graphql-parser`.
pub struct GraphQlEngine;

static LANGUAGES: &[Language] = &[Language::GraphQl];

impl Engine for GraphQlEngine {
    fn name(&self) -> &'static str {
        "graphql"
    }

    fn languages(&self) -> &'static [Language] {
        LANGUAGES
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            lint: true,
            format: true,
            fix: false,
        }
    }

    fn version(&self) -> &str {
        // Tracks the published `graphql-parser` crate version; bump when the
        // dependency is updated so cached output is invalidated.
        "0.4"
    }

    fn lint(&self, src: &SourceFile, _cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        // Try schema parse first; fall back to query parse. A successful parse
        // of either type means no parse-error diagnostics.
        if parse_schema::<&str>(&src.content).is_ok() {
            return Ok(Vec::new());
        }
        if parse_query::<&str>(&src.content).is_ok() {
            return Ok(Vec::new());
        }

        // Both failed. Re-run to capture the error message; pick schema parse
        // error when SDL keywords appear in the content, otherwise query.
        let err_msg = if looks_like_schema(&src.content) {
            match parse_schema::<&str>(&src.content) {
                Err(e) => e.to_string(),
                Ok(_) => return Ok(Vec::new()),
            }
        } else {
            match parse_query::<&str>(&src.content) {
                Err(e) => e.to_string(),
                Ok(_) => return Ok(Vec::new()),
            }
        };

        let span = extract_location(&err_msg).map(|(line, col)| Span {
            start_line: line,
            start_col: col,
            end_line: line,
            end_col: col.saturating_add(1),
        });

        Ok(vec![Diagnostic {
            engine: "graphql".to_string(),
            code: Some("syntax".to_string()),
            severity: Severity::Error,
            message: err_msg,
            span,
            fix: None,
            metadata: Default::default(),
        }])
    }

    fn format(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
        let indent = u32::try_from(cfg.indent_width).unwrap_or(2);
        let mut style = Style::default();
        style.indent(indent);

        let formatted = if let Ok(doc) = parse_schema::<&str>(&src.content) {
            doc.format(&style)
        } else if let Ok(doc) = parse_query::<&str>(&src.content) {
            doc.format(&style)
        } else {
            // Cannot parse: leave the file untouched to avoid data loss.
            return Ok(FormatOutput::Unchanged);
        };

        if formatted == src.content {
            Ok(FormatOutput::Unchanged)
        } else {
            Ok(FormatOutput::Formatted(formatted))
        }
    }
}

/// Returns `true` when the content is likely a Schema Definition Language
/// document. Used to pick the more relevant parse error when both parsers
/// fail.
fn looks_like_schema(content: &str) -> bool {
    const SDL_KEYWORDS: &[&str] = &[
        "type ",
        "interface ",
        "enum ",
        "union ",
        "input ",
        "scalar ",
        "directive ",
        "schema {",
        "extend ",
    ];
    SDL_KEYWORDS.iter().any(|kw| content.contains(kw))
}

/// Extract a 1-based (line, col) pair from a `combine`-style error message.
///
/// The message format emitted by `graphql-parser` is:
/// `"[query|schema] parse error: Parse error at LINE:COL\n..."`.
fn extract_location(err_msg: &str) -> Option<(u32, u32)> {
    // Strip the optional "query parse error: " / "schema parse error: " prefix.
    let msg = err_msg
        .trim_start_matches("query parse error: ")
        .trim_start_matches("schema parse error: ");
    let after = msg.strip_prefix("Parse error at ")?;
    let coords = after.lines().next()?;
    let (line_s, col_s) = coords.split_once(':')?;
    let line: u32 = line_s.trim().parse().ok()?;
    let col: u32 = col_s.trim().parse().ok()?;
    Some((line, col))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_location_from_query_error() {
        let msg = "query parse error: Parse error at 3:7\nUnexpected end of input\nExpected Name\n";
        assert_eq!(extract_location(msg), Some((3, 7)));
    }

    #[test]
    fn extract_location_from_schema_error() {
        let msg = "schema parse error: Parse error at 1:17\nUnexpected `}[Punctuator]`\nExpected Name or [\n";
        assert_eq!(extract_location(msg), Some((1, 17)));
    }

    #[test]
    fn looks_like_schema_detects_type() {
        assert!(looks_like_schema("type User { id: ID! }"));
        assert!(looks_like_schema("interface Node { id: ID! }"));
        assert!(!looks_like_schema("query { user { id } }"));
    }
}
