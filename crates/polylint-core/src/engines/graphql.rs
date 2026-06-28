//! GraphQL backend: formatting via `pretty_graphql`, parse-error lint via
//! `graphql-parser`.
//!
//! Capabilities: [`Capabilities::lint`] (parse-error diagnostics) and
//! [`Capabilities::format`] (Prettier-style canonical output via
//! `pretty_graphql::format_text`).
//!
//! Both GraphQL Schema Definition Language (SDL) files and query/operation
//! files share the `.graphql` / `.gql` extension. For **linting**, the backend
//! tries SDL parsing first (most common in project repositories), then falls
//! back to query document parsing. If both fail, the parse error is surfaced as
//! a [`Diagnostic`].
//!
//! For **formatting**, `pretty_graphql` accepts both SDL and query documents
//! through the same `format_text` entry point. If the document is unparsable
//! the formatter returns an error and we return [`FormatOutput::Unchanged`] to
//! avoid data loss.
//!
//! # Opinionated defaults
//!
//! | Setting | Polylint default | pretty_graphql default |
//! |---|---|---|
//! | `print_width` | 120 | 80 |
//! | `indent_width` | 2 | 2 |
//! | `use_tabs` | false | false |
//!
//! `print_width` follows [`crate::config::GlobalDefaults::line_length`] (default 120) and can
//! be further overridden via `[fmt.graphql.graphql]` in `polylint.toml`. The
//! `indent_width` comes from [`EngineConfig::indent_width`] (itself derived
//! from [`Language::default_indent_width`], which is 2 for GraphQL).

use graphql_parser::query::parse_query;
use graphql_parser::schema::parse_schema;
use pretty_graphql::config::{FormatOptions, LayoutOptions};

use crate::config::EngineConfig;
use crate::engine::{Capabilities, Diagnostic, Engine, FormatOutput, Severity, SourceFile, Span};
use crate::language::Language;

/// GraphQL backend: `pretty_graphql` for formatting, `graphql-parser` for lint.
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
        // Tracks the active formatter; bump when the formatter dep is updated
        // so cached output is invalidated.  Format: `<formatter>-<version>`.
        "pretty_graphql-0.2.3"
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
            fix: vec![],
            metadata: Default::default(),
        }])
    }

    fn format(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
        // `print_width` from globals (default 120); `indent_width` from engine
        // config (default 2 for GraphQL).
        let print_width = cfg.globals.line_length;
        let indent_width = cfg
            .options
            .get("indent_width")
            .and_then(|v| v.as_integer())
            .and_then(|v| usize::try_from(v).ok())
            .unwrap_or(cfg.indent_width);

        let options = FormatOptions {
            layout: LayoutOptions {
                print_width,
                indent_width,
                use_tabs: false,
                ..Default::default()
            },
            ..Default::default()
        };

        let formatted = match pretty_graphql::format_text(&src.content, &options) {
            Ok(s) => s,
            // Unparsable document: leave untouched to avoid data loss.
            Err(_) => return Ok(FormatOutput::Unchanged),
        };

        if formatted == *src.content {
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
