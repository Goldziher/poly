//! Checkstyle's `-f xml` report.
//!
//! ```xml
//! <checkstyle version="12.3.1">
//! <file name="/abs/Foo.java">
//! <error line="3" column="15" severity="error" message="..." source="com.puppycrawl...MethodNameCheck"/>
//! </file>
//! </checkstyle>
//! ```
//!
//! The format is regular and machine-written, so it is read with a small
//! scanner instead of an XML dependency. Only `<file>` and `<error>` carry
//! information; the rest is skipped.

use anyhow::{Result, bail};

use super::Finding;
use crate::engine::Severity;

const END_OF_REPORT: &str = "</checkstyle>";

pub(super) fn parse(stdout: &str) -> Result<Vec<Finding>> {
    if !stdout.contains("<checkstyle") || !stdout.contains(END_OF_REPORT) {
        bail!("checkstyle did not print a complete XML report");
    }
    let mut findings = Vec::new();
    let mut file: Option<String> = None;
    for tag in tags(stdout) {
        match tag.name {
            "file" => file = tag.attribute("name"),
            "/file" => file = None,
            "error" => findings.push(finding(&tag, file.clone())),
            _ => {}
        }
    }
    Ok(findings)
}

fn finding(tag: &Tag<'_>, path: Option<String>) -> Finding {
    let severity = match tag.attribute("severity").as_deref() {
        Some("warning") => Severity::Warning,
        Some("info") => Severity::Info,
        Some("ignore") => Severity::Hint,
        _ => Severity::Error,
    };
    let mut finding = Finding::new(severity, tag.attribute("message").unwrap_or_default());
    finding.path = path;
    finding.line = tag.attribute("line").and_then(|value| value.parse().ok());
    finding.column = tag.attribute("column").and_then(|value| value.parse().ok());
    finding.code = tag.attribute("source").map(|source| rule_name(&source));
    finding
}

/// `com.puppycrawl.tools.checkstyle.checks.naming.MethodNameCheck` -> `MethodName`,
/// the name a user writes in `checkstyle.xml` and in `// CHECKSTYLE.OFF:`.
fn rule_name(source: &str) -> String {
    let class = source.rsplit('.').next().unwrap_or(source);
    class.strip_suffix("Check").unwrap_or(class).to_owned()
}

struct Tag<'a> {
    name: &'a str,
    attributes: &'a str,
}

impl Tag<'_> {
    fn attribute(&self, wanted: &str) -> Option<String> {
        let mut rest = self.attributes;
        while let Some(equals) = rest.find('=') {
            let key = rest[..equals].trim();
            let after = rest[equals + 1..].trim_start();
            let quote = after.chars().next().filter(|c| *c == '"' || *c == '\'')?;
            let body = &after[1..];
            let end = body.find(quote)?;
            if key == wanted {
                return Some(unescape(&body[..end]));
            }
            rest = &body[end + 1..];
        }
        None
    }
}

/// Every element tag of the document, in order, skipping the prolog, comments
/// and text.
fn tags(document: &str) -> impl Iterator<Item = Tag<'_>> {
    let mut rest = document;
    std::iter::from_fn(move || {
        loop {
            let start = rest.find('<')?;
            let body = &rest[start + 1..];
            if body.starts_with('?') || body.starts_with('!') {
                rest = &body[body.find('>')? + 1..];
                continue;
            }
            let end = tag_end(body)?;
            rest = &body[end + 1..];
            let inner = body[..end].trim_end_matches('/');
            let (name, attributes) = inner.split_once(char::is_whitespace).unwrap_or((inner, ""));
            return Some(Tag { name, attributes });
        }
    })
}

/// Index of the `>` closing the tag in `body`, ignoring any inside an attribute
/// value (`message="a > b"`).
fn tag_end(body: &str) -> Option<usize> {
    let mut quote: Option<char> = None;
    for (index, c) in body.char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(open), c) if c == open => quote = None,
            (None, '>') => return Some(index),
            _ => {}
        }
    }
    None
}

fn unescape(text: &str) -> String {
    if !text.contains('&') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let tail = &rest[amp..];
        let Some(semi) = tail.find(';') else {
            out.push_str(tail);
            return out;
        };
        match entity(&tail[1..semi]) {
            Some(c) => out.push(c),
            None => out.push_str(&tail[..=semi]),
        }
        rest = &tail[semi + 1..];
    }
    out.push_str(rest);
    out
}

fn entity(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => {
            let digits = name.strip_prefix('#')?;
            let code = match digits.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => digits.parse().ok()?,
            };
            char::from_u32(code)
        }
    }
}
