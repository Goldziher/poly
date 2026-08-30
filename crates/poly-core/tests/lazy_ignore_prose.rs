//! `lazy-ignore` must not fire on prose that *documents* a suppression marker.
//!
//! The rule is a line scan for `# noqa`, `// eslint-disable`, `// oxlint-disable`
//! and `// biome-ignore` written without a justification. In a source file that
//! is exactly right. In Markdown it is exactly wrong: every occurrence is either
//! prose explaining the syntax, a table cell naming it, or a fenced example
//! showing it — never a directive that suppresses anything, because nothing
//! lints the prose it sits in.
//!
//! poly's own documentation is the victim, which is the canonical signal that a
//! rule has outrun its evidence: the configuration guide describes what
//! `lazy-ignore` does, and `poly lint` reported that description as a lazy
//! ignore. The same class was already fixed once for Go templates —
//! `engines/template.rs` grew `contains_go_template_markdown` because prose
//! documenting `{{ … }}` inside a code construct tripped rumdl, and poly's own
//! `CHANGELOG.md` was the file that exposed it.
//!
//! The rule must keep firing everywhere it earns its keep, so the second test
//! pins a real Python suppression rather than only asserting the silence.

use poly_core::{Config, RunOptions};

fn options() -> RunOptions {
    RunOptions {
        no_cache: true,
        jobs: Some(1),
        exclude: Vec::new(),
        force_exclude: false,
        fix_generated: false,
        generated: None,
        explicit_config: true,
        config_resolver: None,
        externally_linted_languages: Vec::new(),
    }
}

/// Every `lazy-ignore` finding produced for a file named `name` holding `body`.
fn lazy_ignores(name: &str, body: &str) -> Vec<String> {
    let directory = tempfile::tempdir().expect("temporary repository");
    let path = directory.path().join(name);
    std::fs::write(&path, body).expect("write fixture");

    let run = poly_core::lint_run(&[path], &Config::default(), &options(), false, false).expect("lint run completes");

    run.results
        .iter()
        .flat_map(|result| &result.diagnostics)
        .filter(|d| d.code.as_deref() == Some("lazy-ignore"))
        .map(|d| d.title.clone())
        .collect()
}

/// The shapes that actually appear in poly's own docs: a fenced example, a
/// backticked span in a sentence, and a Markdown table cell.
const DOCUMENTATION: &str = "\
# Suppression

`RUF100` fires on any `# noqa` code poly does not select.

| Rule | Why |
|---|---|
| `lazy-ignore` | reports a bare `# noqa` or `// eslint-disable` with no reason |

```python
# noqa
```

Write `// biome-ignore` with a justification after it.
";

#[test]
fn markdown_documenting_a_suppression_marker_is_not_a_lazy_ignore() {
    let found = lazy_ignores("CONFIGURATION.md", DOCUMENTATION);

    assert!(
        found.is_empty(),
        "prose describing a suppression marker is documentation, not a suppression: {found:?}"
    );
}

#[test]
fn mdx_is_covered_too() {
    let found = lazy_ignores("guide.mdx", DOCUMENTATION);
    assert!(found.is_empty(), "MDX is prose as much as Markdown is: {found:?}");
}

/// The carve-out must be about prose, not about the marker. A real unjustified
/// suppression in real source is the entire point of the rule, so this pins that
/// the fix did not simply disable it.
#[test]
fn a_bare_suppression_in_source_is_still_reported() {
    let found = lazy_ignores("app.py", "import os  # noqa\n\nx = os.getcwd()\n");

    assert_eq!(
        found.len(),
        1,
        "an unjustified `# noqa` in Python source must still be reported, got {found:?}"
    );
}

/// And a justified one still must not be, so the fix did not widen the rule
/// either.
#[test]
fn a_justified_suppression_in_source_is_not_reported() {
    let found = lazy_ignores("app.py", "import os  # noqa: F401 kept for re-export\n");

    assert!(found.is_empty(), "a justified suppression is fine: {found:?}");
}
