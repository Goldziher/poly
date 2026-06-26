//! Opinionated default behavior shared across backends.
//!
//! The base layer is whitespace normalization, applied by the reference backend
//! and reusable as a final pass for any formatter: normalize line endings, trim
//! trailing whitespace, collapse trailing blank lines, enforce a single final
//! newline.

use crate::config::GlobalDefaults;

/// Normalize whitespace per the global defaults. Idempotent.
pub fn normalize_whitespace(text: &str, g: &GlobalDefaults) -> String {
    let unified = text.replace("\r\n", "\n");
    let mut lines: Vec<String> = unified
        .split('\n')
        .map(|l| {
            if g.trim_trailing_whitespace {
                l.trim_end().to_string()
            } else {
                l.to_string()
            }
        })
        .collect();

    // Drop trailing empty lines; a single final newline is re-added below.
    while matches!(lines.last(), Some(l) if l.is_empty()) {
        lines.pop();
    }

    let nl = g.line_ending.as_str();
    let mut out = lines.join(nl);
    if g.final_newline && !out.is_empty() {
        out.push_str(nl);
    }
    out
}
