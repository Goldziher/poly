//! File-filter translation for hook configs: lefthook `glob` / `exclude` and
//! pre-commit `files` / `exclude` regular expressions become the glob lists
//! poly's `[hooks]` jobs and builtins accept.
//!
//! Globs pass through verbatim (poly compiles them with `globset`, which
//! matches lefthook's `*.py` at any depth). Regexes are translated only when
//! they are a plain alternation of literals, `.*` runs and groups; anything
//! else (character classes, quantifiers on literals, lookaround) is refused so
//! the caller can report the filter instead of guessing at its meaning.

/// Upper bound on the alternatives one regex may expand to.
const MAX_ALTERNATIVES: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Node {
    Lit(char),
    AnyRun,
    AnyPlus,
    Start,
    End,
    Group(Vec<Vec<Node>>),
}

/// Translate a lefthook `glob` / `exclude` string into globs.
///
/// Lefthook accepts globs and, for `exclude`, legacy regexes; a string with
/// regex-only syntax is routed through [`regex_to_globs`].
pub(super) fn filter_to_globs(raw: &str) -> Result<Vec<String>, String> {
    if looks_like_regex(raw) {
        regex_to_globs(raw)
    } else {
        Ok(vec![raw.to_string()])
    }
}

fn looks_like_regex(raw: &str) -> bool {
    raw.contains('\\')
        || raw.starts_with('^')
        || raw.ends_with('$')
        || raw.contains(".*")
        || raw.contains("(?")
        || raw.contains('|')
}

/// Translate a regular expression (pre-commit `files` / `exclude` semantics:
/// an unanchored search over the repo-relative path) into an equivalent list of
/// globs, one per top-level alternative.
///
/// A literal `.` is read as a literal dot rather than "any character": the
/// difference only matters for a file named with another character in that
/// position, and the literal reading is what the author wrote.
pub(super) fn regex_to_globs(pattern: &str) -> Result<Vec<String>, String> {
    let mut parser = Parser {
        chars: normalize_verbose(pattern).chars().collect(),
        pos: 0,
    };
    let alternatives = parser.alternatives(false)?;
    if parser.pos < parser.chars.len() {
        return Err(format!("unbalanced `)` in `{}`", pattern.trim()));
    }
    let mut globs: Vec<String> = Vec::new();
    for alternative in expand_alternatives(&alternatives)? {
        let glob = render(&alternative)?;
        if !globs.contains(&glob) {
            globs.push(glob);
        }
    }
    Ok(globs)
}

/// Strip a leading `(?x)` verbose flag and, when present, the whitespace and
/// `#` comments it makes insignificant.
fn normalize_verbose(pattern: &str) -> String {
    let trimmed = pattern.trim_start();
    let Some(body) = trimmed.strip_prefix("(?x)") else {
        return pattern.trim().to_string();
    };
    let mut out = String::new();
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                out.push(c);
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            }
            '#' => {
                for rest in chars.by_ref() {
                    if rest == '\n' {
                        break;
                    }
                }
            }
            c if c.is_whitespace() => {}
            c => out.push(c),
        }
    }
    out
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn alternatives(&mut self, in_group: bool) -> Result<Vec<Vec<Node>>, String> {
        let mut alternatives = vec![Vec::new()];
        while let Some(&c) = self.chars.get(self.pos) {
            match c {
                '|' => {
                    self.pos += 1;
                    alternatives.push(Vec::new());
                }
                ')' => {
                    if in_group {
                        return Ok(alternatives);
                    }
                    return Err("unbalanced `)`".to_string());
                }
                _ => {
                    let node = self.atom()?;
                    alternatives.last_mut().expect("one alternative").push(node);
                }
            }
        }
        if in_group {
            return Err("unterminated `(` group".to_string());
        }
        Ok(alternatives)
    }

    fn atom(&mut self) -> Result<Node, String> {
        let c = self.chars[self.pos];
        self.pos += 1;
        let node = match c {
            '^' => Node::Start,
            '$' => Node::End,
            '(' => {
                if self.chars.get(self.pos) == Some(&'?') {
                    if self.chars.get(self.pos + 1) != Some(&':') {
                        return Err("regex flags or lookaround are not translatable".to_string());
                    }
                    self.pos += 2;
                }
                let alternatives = self.alternatives(true)?;
                self.pos += 1;
                Node::Group(alternatives)
            }
            '[' => return Err("character classes are not translatable".to_string()),
            '\\' => {
                let escaped = *self.chars.get(self.pos).ok_or("dangling `\\`")?;
                self.pos += 1;
                if escaped.is_ascii_alphanumeric() {
                    return Err(format!("class escape `\\{escaped}` is not translatable"));
                }
                Node::Lit(escaped)
            }
            '.' => match self.chars.get(self.pos) {
                Some('*') => {
                    self.pos += 1;
                    Node::AnyRun
                }
                Some('+') => {
                    self.pos += 1;
                    Node::AnyPlus
                }
                _ => Node::Lit('.'),
            },
            '*' | '+' | '?' | '{' => {
                return Err(format!("quantifier `{c}` on a literal is not translatable"));
            }
            other => Node::Lit(other),
        };
        match self.chars.get(self.pos) {
            Some('?') if !matches!(node, Node::Start | Node::End) => {
                self.pos += 1;
                Ok(Node::Group(vec![vec![node], Vec::new()]))
            }
            Some('*' | '+' | '?' | '{') => Err("quantifier is not translatable".to_string()),
            _ => Ok(node),
        }
    }
}

fn expand_alternatives(alternatives: &[Vec<Node>]) -> Result<Vec<Vec<Node>>, String> {
    let mut out = Vec::new();
    for alternative in alternatives {
        out.extend(expand(alternative, vec![Vec::new()])?);
        if out.len() > MAX_ALTERNATIVES {
            return Err("pattern expands to too many alternatives".to_string());
        }
    }
    Ok(out)
}

fn expand(nodes: &[Node], prefixes: Vec<Vec<Node>>) -> Result<Vec<Vec<Node>>, String> {
    let mut acc = prefixes;
    for node in nodes {
        acc = match node {
            Node::Group(alternatives) => {
                let mut next = Vec::new();
                for alternative in alternatives {
                    for expanded in expand(alternative, vec![Vec::new()])? {
                        for prefix in &acc {
                            let mut combined = prefix.clone();
                            combined.extend(expanded.iter().cloned());
                            next.push(combined);
                        }
                    }
                }
                next
            }
            leaf => {
                for prefix in &mut acc {
                    prefix.push(leaf.clone());
                }
                acc
            }
        };
        if acc.len() > MAX_ALTERNATIVES {
            return Err("pattern expands to too many alternatives".to_string());
        }
    }
    Ok(acc)
}

fn render(alternative: &[Node]) -> Result<String, String> {
    let anchored_start = alternative.first() == Some(&Node::Start);
    let anchored_end = alternative.last() == Some(&Node::End);
    let body = &alternative[usize::from(anchored_start)..alternative.len() - usize::from(anchored_end)];
    let mut glob = String::new();
    if !anchored_start {
        glob.push('*');
    }
    for node in body {
        match node {
            Node::Lit(c) => push_literal(&mut glob, *c),
            Node::AnyRun => glob.push('*'),
            Node::AnyPlus => glob.push_str("?*"),
            Node::Start | Node::End => {
                return Err("`^` / `$` anchor in the middle of a pattern is not translatable".to_string());
            }
            Node::Group(_) => unreachable!("groups are expanded before rendering"),
        }
    }
    if !anchored_end {
        glob.push('*');
    }
    Ok(collapse_stars(&glob))
}

fn push_literal(glob: &mut String, c: char) {
    if matches!(c, '*' | '?' | '[' | ']' | '{' | '}' | '\\') {
        glob.push('\\');
    }
    glob.push(c);
}

fn collapse_stars(glob: &str) -> String {
    let mut out = String::with_capacity(glob.len());
    for c in glob.chars() {
        if c == '*' && out.ends_with('*') {
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_globs_pass_through() {
        assert_eq!(filter_to_globs("playground/**").unwrap(), ["playground/**"]);
        assert_eq!(filter_to_globs("*.{py,pyi}").unwrap(), ["*.{py,pyi}"]);
    }

    #[test]
    fn escaped_dot_regex_becomes_a_literal_path() {
        assert_eq!(
            filter_to_globs(r"armis/public_sdks/python/public/pyproject\.toml").unwrap(),
            ["*armis/public_sdks/python/public/pyproject.toml*"]
        );
    }

    #[test]
    fn anchored_alternation_expands_one_glob_per_branch() {
        let globs = regex_to_globs(r"^(thirdparty/pip/.*-lock\..*_py3\.txt|products/rms/.*\.txt)$").unwrap();
        assert_eq!(globs, ["thirdparty/pip/*-lock.*_py3.txt", "products/rms/*.txt"]);
    }

    #[test]
    fn verbose_multiline_regex_is_flattened() {
        let globs = regex_to_globs("(?x)(\n    ^a/b\\.cfg$|\n    \\.(tar|gz)$\n)\n").unwrap();
        assert_eq!(globs, ["a/b.cfg", "*.tar", "*.gz"]);
    }

    #[test]
    fn unanchored_regex_matches_anywhere() {
        assert_eq!(
            regex_to_globs(r"^products/em/backend/").unwrap(),
            ["products/em/backend/*"]
        );
    }

    #[test]
    fn optional_literal_or_group_expands_both_ways() {
        let globs = regex_to_globs(r"^util/vmlinux(_arm64)?\.h$").unwrap();
        assert_eq!(globs, ["util/vmlinux_arm64.h", "util/vmlinux.h"]);
        assert_eq!(regex_to_globs(r"^ab?c$").unwrap(), ["abc", "ac"]);
    }

    #[test]
    fn untranslatable_syntax_is_refused() {
        for pattern in [r"^[a-z]+\.py$", r"^a\d+$", r"^(?!x)a$", r"^ab+c$", r"^(a|b"] {
            assert!(regex_to_globs(pattern).is_err(), "{pattern} must be refused");
        }
    }
}
