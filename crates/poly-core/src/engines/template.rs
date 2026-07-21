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
//! two common look-alikes that must keep being processed:
//! - GitHub Actions `${{ … }}` expressions — valid YAML scalars (the `{{` is
//!   `$`-prefixed).
//! - MDX / JSX object literals such as `style={{ color: "red" }}` — a bare `{{`
//!   with no Go-template action marker.

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

#[cfg(test)]
mod tests {
    use super::contains_go_template;

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
    fn ignores_mdx_object_literals_and_plain_content() {
        assert!(!contains_go_template("<Note style={{ color: \"red\" }}>hi</Note>\n"));
        assert!(!contains_go_template("# Heading\n\nPlain markdown with no braces.\n"));
        assert!(!contains_go_template("key: value\nlist:\n  - a\n  - b\n"));
    }
}
