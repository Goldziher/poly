//! Insta snapshot fixtures for the HCL / Terraform backend.
//!
//! Three fixtures:
//! - `known_bad_lint_snapshot` — a `.tf` file with a syntax error asserts the
//!   expected `syntax-error` [`Diagnostic`] at the correct line.
//! - `known_unformatted_snapshot` — a comment-free messy `.tf` file asserts
//!   exact normalized output produced by the `hcl-rs` formatter.
//! - `commented_file_preserves_comments` — a `.tf` file with comments asserts
//!   that comments survive the format pass (tier-2 delegation safety guarantee).

use polylint_core::{
    Language,
    config::{EngineConfig, GlobalDefaults},
    engine::{Engine, FormatOutput, Severity, SourceFile},
    engines::hcl::HclEngine,
};

fn engine_cfg() -> EngineConfig {
    EngineConfig {
        globals: GlobalDefaults::default(),
        // HCL/Terraform canonical indent is 2 spaces.
        indent_width: 2,
        options: toml::Table::new(),
    }
}

fn make_src(path: &str, content: &str) -> SourceFile {
    SourceFile {
        path: path.into(),
        language: Language::Hcl,
        content: content.into(),
    }
}

// ---------------------------------------------------------------------------
// Known-bad fixture: expects a syntax-error diagnostic
// ---------------------------------------------------------------------------

/// Intentionally invalid Terraform: unclosed block body.
const KNOWN_BAD: &str = "\
resource \"aws_instance\" \"bad\" {
  ami           = \"ami-12345678\"
  instance_type = \"t2.micro\"
";

#[test]
fn known_bad_lint_snapshot() {
    let engine = HclEngine;
    let src = make_src("known_bad.tf", KNOWN_BAD);
    let diags = engine.lint(&src, &engine_cfg()).unwrap();

    assert!(!diags.is_empty(), "expected at least one diagnostic for invalid HCL");

    let first = &diags[0];
    assert_eq!(
        first.code.as_deref(),
        Some("syntax-error"),
        "first diagnostic must have code syntax-error"
    );
    assert_eq!(first.severity, Severity::Error);
    let span = first.span.expect("syntax-error must carry a span");
    assert!(
        span.start_line >= 1,
        "span start_line must be 1-based, got {}",
        span.start_line
    );

    // Stable snapshot: (code, start_line).
    let summary: Vec<_> = diags
        .iter()
        .map(|d| (d.code.as_deref().unwrap_or(""), d.span.as_ref().map(|s| s.start_line)))
        .collect();
    insta::assert_debug_snapshot!("hcl_known_bad_diagnostics", summary);
}

// ---------------------------------------------------------------------------
// Known-unformatted fixture: expects exact formatted output (no comments)
// ---------------------------------------------------------------------------

/// Messy Terraform that hcl-rs should normalize: inconsistent spacing around
/// `=`, mixed attribute alignment.
const KNOWN_UNFORMATTED: &str = "\
resource   \"aws_s3_bucket\"   \"example\" {
  bucket =  \"my-example-bucket\"
  acl  =   \"private\"

  tags = {
    Name        = \"example\"
    Environment = \"prod\"
  }
}
";

#[test]
fn known_unformatted_snapshot() {
    let engine = HclEngine;
    let src = make_src("known_unformatted.tf", KNOWN_UNFORMATTED);
    let result = engine.format(&src, &engine_cfg()).unwrap();

    let formatted = match result {
        FormatOutput::Formatted(s) => s,
        // If hcl-rs considers it already formatted, use the original.
        FormatOutput::Unchanged => KNOWN_UNFORMATTED.to_string(),
    };

    // Must still parse cleanly after formatting.
    let post_diags = engine
        .lint(&make_src("known_unformatted.tf", &formatted), &engine_cfg())
        .unwrap();
    assert!(
        post_diags.is_empty(),
        "formatted output must parse without errors: {formatted}"
    );

    insta::assert_snapshot!("hcl_known_unformatted_output", formatted);
}

// ---------------------------------------------------------------------------
// Comment-safety fixture: comments must survive a format pass
// ---------------------------------------------------------------------------

/// A `.tf` file with both a leading comment and an inline `//` comment.
/// After formatting via the tier-2 delegation path, every comment must still
/// be present in the output — byte-identical when tier-2 is a no-op, or at
/// minimum comment-preserving when it reindents.
const WITH_COMMENTS: &str = "\
# This is a leading comment
resource \"aws_instance\" \"web\" {
  // inline comment
  ami           = \"ami-12345678\"
  instance_type = \"t2.micro\"
}
";

#[test]
fn commented_file_preserves_comments() {
    let engine = HclEngine;
    let src = make_src("with_comments.tf", WITH_COMMENTS);
    let result = engine.format(&src, &engine_cfg()).unwrap();

    let output = match result {
        FormatOutput::Unchanged => WITH_COMMENTS.to_string(),
        FormatOutput::Formatted(s) => s,
    };

    assert!(
        output.contains("# This is a leading comment"),
        "leading hash comment must be preserved; got:\n{output}"
    );
    assert!(
        output.contains("// inline comment"),
        "inline // comment must be preserved; got:\n{output}"
    );
}
