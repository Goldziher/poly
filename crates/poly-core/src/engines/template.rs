//! Detection of Go / Helm template syntax embedded in otherwise-structured files.
//!
//! Helm charts ship Go-templated YAML (and, less often, templated Markdown):
//! `{{ .Values.x }}`, `{{- if … }}`, `{{/* … */}}`. That text is not valid YAML
//! or clean Markdown, so the strict backends (`yaml`, `rumdl`) report spurious
//! syntax errors on it. Rather than key off chart layout (`Chart.yaml`,
//! `templates/`), the backends scan file **content** for Go-template actions and
//! skip a file that contains them.
//!
//! The check is deliberately specific to Go templates so it does not fire on the
//! two common lookalikes that must keep being processed:
//! - GitHub Actions `${{ … }}` expressions — valid YAML scalars (the `{{` is
//!   `$`-prefixed).
//! - MDX / JSX object literals such as `style={{ color: "red" }}` — a bare `{{`
//!   with no Go-template action marker.
//!
//! Markdown adds a third lookalike, which is why it gets its own entry point
//! ([`contains_go_template_markdown`]): prose that *documents* template syntax
//! inside a code construct. poly's own `CHANGELOG.md` is the canonical victim —
//! it shows `{{.CLI_ARGS}}` in a fenced block and `` `{{- … }}` `` in inline
//! spans, and was therefore excluded from formatting wholesale. Code spans and
//! code blocks are rendered verbatim, so template actions inside them are
//! documentation, not a template.

/// Go-template action keywords that, when they open a `{{ … }}` block, mark the
/// content as a Go / Helm template rather than an incidental `{{`.
const GO_TEMPLATE_KEYWORDS: &[&str] = &[
    "if", "range", "end", "else", "with", "define", "block", "template", "include", "printf", "tpl", "toYaml",
    "required", "default", "quote", "nindent", "indent",
];

/// Whether `content` contains Go / Helm template syntax.
///
/// Returns `true` when it finds a `{{ … }}` opening that is a Go-template action:
/// a trim marker (`{{-`), a template comment (`{{/*`), field/variable access
/// (`{{ .` / `{{ $`), or one of [`GO_TEMPLATE_KEYWORDS`]. A `{{` immediately
/// preceded by `$` (GitHub Actions `${{ }}`) is ignored, as is a bare `{{` with
/// no action marker (MDX/JSX object literals).
/// Reason reported when a file is skipped for carrying Go/Helm template actions.
pub(crate) const GO_TEMPLATE_SKIP: &str = "Go/Helm template syntax";

pub(crate) fn contains_go_template(content: &str) -> bool {
    let bytes = content.as_bytes();
    let mut search_from = 0;
    while let Some(offset) = content[search_from..].find("{{") {
        let open = search_from + offset;
        let after = open + 2;
        search_from = after;
        // GitHub Actions `${{ … }}` — a valid YAML scalar, must not be skipped.
        if open > 0 && bytes[open - 1] == b'$' {
            continue;
        }
        let tail = &content[after..];
        if is_go_template_action(tail) {
            return true;
        }
    }
    false
}

/// Whether the text immediately following a `{{` opens a Go-template action.
fn is_go_template_action(tail: &str) -> bool {
    // Trim marker (`{{-`) and comment (`{{/*`) attach directly to the braces.
    if tail.starts_with('-') || tail.starts_with("/*") {
        return true;
    }
    let body = tail.trim_start();
    if body.starts_with('.') || body.starts_with('$') || body.starts_with("/*") {
        return true;
    }
    GO_TEMPLATE_KEYWORDS.iter().any(|kw| starts_with_keyword(body, kw))
}

/// Whether `body` starts with `keyword` followed by a word boundary (whitespace,
/// end of string, or a closing brace) — so `range` matches but `ranger` does not.
fn starts_with_keyword(body: &str, keyword: &str) -> bool {
    body.strip_prefix(keyword)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(|c: char| c.is_whitespace() || c == '}' || c == '-'))
}

/// Minimum number of backticks/tildes that open or close a fenced code block.
const MIN_FENCE_LEN: usize = 3;

/// Maximum leading spaces a fence marker may carry before it stops being a fence
/// (CommonMark allows up to three; a fourth makes it an indented code block).
const MAX_FENCE_INDENT: usize = 3;

/// Leading spaces that start an indented code block.
const INDENTED_CODE_INDENT: usize = 4;

/// An open fenced code block: its marker character and run length. A fence is
/// closed only by a run of the *same* character that is at least as long.
struct Fence {
    marker: u8,
    length: usize,
    /// Byte offset of the first line after the opening fence, so an unterminated
    /// fence can be re-scanned as prose (see [`contains_go_template_markdown`]).
    content_start: usize,
}

/// Whether `content` contains Go / Helm template syntax **outside** Markdown code.
///
/// Same detection as [`contains_go_template`], but template actions appearing in
/// a fenced code block, an indented code block, or an inline code span are
/// treated as documentation of template syntax rather than as a template. An
/// action in live prose (a Helm chart's templated README, say) still counts.
///
/// Ambiguity is always resolved toward *skipping*: declining to format a file
/// costs nothing, while reflowing a real template destroys it. Two consequences
/// follow from that rule:
/// - An **unterminated fence** does not make the rest of the file code. The
///   document is malformed, so its remainder is re-scanned as live prose and a
///   template action there still triggers the skip. (CommonMark would render it
///   as code; that is the unsafe direction here.)
/// - An **unterminated inline span** likewise leaves the rest of its line prose.
///
/// This is Markdown/MDX-only. YAML has no code constructs — backticks and
/// fence-looking lines there are ordinary scalar text — so `yaml.rs` keeps
/// calling [`contains_go_template`] unchanged.
pub(crate) fn contains_go_template_markdown(content: &str) -> bool {
    let mut fence: Option<Fence> = None;
    // An indented code block cannot interrupt a paragraph (CommonMark), so an
    // indented continuation line of a paragraph stays prose.
    let mut in_paragraph = false;

    for (offset, line) in lines_with_offsets(content) {
        if let Some(open) = &fence {
            if closes_fence(line, open) {
                fence = None;
            }
            continue;
        }
        if let Some(open) = opens_fence(line, offset) {
            fence = Some(open);
            in_paragraph = false;
            continue;
        }
        if line.trim().is_empty() {
            in_paragraph = false;
            continue;
        }
        if !in_paragraph && is_indented_code(line) {
            continue;
        }
        in_paragraph = true;
        if prose_contains_go_template(line) {
            return true;
        }
    }

    // Unterminated fence: re-scan its body as prose (conservative direction).
    fence.is_some_and(|open| content[open.content_start..].lines().any(prose_contains_go_template))
}

/// Iterate `(byte offset, line)` pairs, with line terminators stripped.
fn lines_with_offsets(content: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut offset = 0;
    content.split_inclusive('\n').map(move |raw| {
        let start = offset;
        offset += raw.len();
        (start, raw.trim_end_matches('\n').trim_end_matches('\r'))
    })
}

/// Split a fence marker line into `(marker, run length, info string)`.
fn fence_run(line: &str) -> Option<(u8, usize, &str)> {
    let rest = line.trim_start_matches(' ');
    if line.len() - rest.len() > MAX_FENCE_INDENT {
        return None;
    }
    let marker = match rest.as_bytes().first() {
        Some(b'`') => b'`',
        Some(b'~') => b'~',
        _ => return None,
    };
    let length = rest.bytes().take_while(|byte| *byte == marker).count();
    (length >= MIN_FENCE_LEN).then(|| (marker, length, &rest[length..]))
}

/// Whether `line` opens a fenced code block, given its byte offset in the file.
fn opens_fence(line: &str, offset: usize) -> Option<Fence> {
    let (marker, length, info) = fence_run(line)?;
    // A backtick fence's info string may not contain a backtick — that rules out
    // an inline code span being mistaken for a fence opener.
    if marker == b'`' && info.contains('`') {
        return None;
    }
    Some(Fence {
        marker,
        length,
        content_start: offset + line.len(),
    })
}

/// Whether `line` closes `open`: the same marker, at least as long, nothing else.
fn closes_fence(line: &str, open: &Fence) -> bool {
    fence_run(line)
        .is_some_and(|(marker, length, info)| marker == open.marker && length >= open.length && info.trim().is_empty())
}

/// Whether `line` is an indented code block line (four spaces or a tab).
fn is_indented_code(line: &str) -> bool {
    line.starts_with('\t') || line.len() - line.trim_start_matches(' ').len() >= INDENTED_CODE_INDENT
}

/// Whether a prose line carries a Go-template action outside its inline code spans.
///
/// Each non-span segment is scanned on its own, so a `$` on one side of a span
/// can never be paired with a `{{` on the other.
fn prose_contains_go_template(line: &str) -> bool {
    let bytes = line.as_bytes();
    let mut index = 0;
    let mut segment_start = 0;
    while index < bytes.len() {
        if bytes[index] != b'`' {
            index += 1;
            continue;
        }
        let run = backtick_run(bytes, index);
        if contains_go_template(&line[segment_start..index]) {
            return true;
        }
        match closing_backtick_run(bytes, index + run, run) {
            Some(close) => {
                index = close + run;
                segment_start = index;
            }
            // Unterminated span — the rest of the line is prose, not code.
            None => return contains_go_template(&line[index..]),
        }
    }
    contains_go_template(&line[segment_start..])
}

/// Length of the backtick run starting at `start`.
fn backtick_run(bytes: &[u8], start: usize) -> usize {
    bytes[start..].iter().take_while(|byte| **byte == b'`').count()
}

/// Offset of the next backtick run of *exactly* `run` backticks at or after `from`.
fn closing_backtick_run(bytes: &[u8], from: usize, run: usize) -> Option<usize> {
    let mut index = from;
    while index < bytes.len() {
        if bytes[index] != b'`' {
            index += 1;
            continue;
        }
        let length = backtick_run(bytes, index);
        if length == run {
            return Some(index);
        }
        index += length;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{contains_go_template, contains_go_template_markdown};

    #[test]
    fn detects_helm_field_access() {
        assert!(contains_go_template("image: {{ .Values.image }}\n"));
        assert!(contains_go_template("replicas: {{.Values.replicaCount}}\n"));
    }

    #[test]
    fn detects_trim_markers_and_control_flow() {
        assert!(contains_go_template(
            "{{- if .Values.enabled }}\nfoo: bar\n{{- end }}\n"
        ));
        assert!(contains_go_template(
            "{{ range .Values.items }}\n- {{ . }}\n{{ end }}\n"
        ));
    }

    #[test]
    fn detects_template_comment_and_helpers() {
        assert!(contains_go_template("{{/* a comment */}}\n"));
        assert!(contains_go_template("data: {{ include \"chart.labels\" . }}\n"));
        assert!(contains_go_template("value: {{ $var }}\n"));
    }

    #[test]
    fn ignores_github_actions_expressions() {
        assert!(!contains_go_template("if: ${{ github.event_name == 'push' }}\n"));
        assert!(!contains_go_template("run: echo ${{ steps.x.outputs.y }}\n"));
    }

    #[test]
    fn markdown_scan_ignores_fenced_indented_and_inline_code() {
        // poly's own CHANGELOG shape: every action lives inside code.
        assert!(!contains_go_template_markdown(
            "# Changelog\n\n- Example:\n\n  ```console\n  $ poly fmt --check Taskfile.yaml  \
             # skipped: contains {{.CLI_ARGS}}\n  ```\n\n- Actions (`{{ .Values.x }}`, \
             `{{- if … }}`, `{{/* … */}}`) are detected by content.\n"
        ));
        assert!(!contains_go_template_markdown(
            "Example:\n\n    image: {{ .Values.image }}\n"
        ));
        assert!(!contains_go_template_markdown("Use `{{ .Values.image }}` here.\n"));
    }

    #[test]
    fn markdown_scan_still_detects_live_template_actions() {
        assert!(contains_go_template_markdown("# {{ .Chart.Name }}\n"));
        // Closed fence: the action after it is live prose.
        assert!(contains_go_template_markdown(
            "```\n{{ .Values.a }}\n```\n\nLive {{ .Values.b }}\n"
        ));
        // An indented code block cannot interrupt a paragraph.
        assert!(contains_go_template_markdown(
            "A wrapped paragraph\n    {{ .Values.x }}\n"
        ));
    }

    #[test]
    fn markdown_scan_handles_longer_fences_and_info_strings() {
        // A three-backtick run does not close a four-backtick fence.
        assert!(!contains_go_template_markdown(
            "````markdown\n```yaml\nimage: {{ .Values.image }}\n```\n````\n"
        ));
        assert!(!contains_go_template_markdown("~~~yaml\n{{- if .x }}\n~~~\n"));
        // A backtick fence's info string may not contain a backtick, so an
        // inline span is never mistaken for a fence opener.
        assert!(contains_go_template_markdown("``x`` and {{ .Values.x }}\n"));
    }

    #[test]
    fn markdown_scan_treats_unterminated_code_as_prose() {
        // Conservative: an unclosed fence must not hide a real template.
        assert!(contains_go_template_markdown("```yaml\nimage: {{ .Values.image }}\n"));
        assert!(contains_go_template_markdown("A stray ` then {{ .Values.x }} live.\n"));
    }

    #[test]
    fn markdown_scan_keeps_the_existing_carve_outs() {
        assert!(!contains_go_template_markdown(
            "if: ${{ github.event_name == 'push' }}\n"
        ));
        assert!(!contains_go_template_markdown(
            "<Note style={{ color: \"red\" }}>hi</Note>\n"
        ));
    }

    #[test]
    fn ignores_mdx_object_literals_and_plain_content() {
        assert!(!contains_go_template("<Note style={{ color: \"red\" }}>hi</Note>\n"));
        assert!(!contains_go_template("# Heading\n\nPlain markdown with no braces.\n"));
        assert!(!contains_go_template("key: value\nlist:\n  - a\n  - b\n"));
    }
}
