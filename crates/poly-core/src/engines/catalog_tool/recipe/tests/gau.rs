//! The shipped `gau` catalog entry, driven by a fake tool script.

use super::*;

/// The shipped `gau` entry end to end, with a stand-in for gau 0.4.0 that
/// refuses any argv but the catalog's and replays a captured report: the
/// workflow with a full-tag and a SHA ref gets both findings on their lines,
/// the workflow whose only ref is `@v5` gets none, in one invocation.
#[test]
fn gau_lint_pins_flags_the_bad_refs_and_passes_the_clean_workflow() {
    let fx = Fixture::new();
    let report = fx.dir.path().join("report.json");
    std::fs::write(&report, include_str!("../output/fixtures/gau_lint_pins.json")).expect("report");
    let gau = fx.script(
        "gau",
        &format!(
            "#!/bin/sh\necho \"$PWD $*\" >> \"$POLY_TEST_LOG\"\n\
             [ \"$1 $2 $3\" = '--lint-pins --format json' ] || exit 2\ncat '{}'\nexit 1\n",
            report.display()
        ),
    );
    let files = vec![
        fx.file(
            ".github/workflows/ci.yml",
            "steps:\n  - uses: actions/setup-node@v4.1.0\n",
        ),
        fx.file(".github/workflows/clean.yml", "steps:\n  - uses: actions/checkout@v5\n"),
    ];
    let tool = poly_catalog::Catalog::get().tool("gau").expect("gau is in the catalog");
    let recipe = tool.recipe.as_ref().expect("gau has a recipe");
    let mut built = RecipeTool::build(tool, recipe, &fx.config(gau), Role::Lint, None).expect("builds");
    built.cwd = std::fs::canonicalize(fx.dir.path()).expect("canonical");

    let results = built.lint_files(&refs(&files));

    assert_eq!(fx.invocations(), 1, "both workflows in one invocation");
    let bad = results[0].as_ref().expect("ran");
    assert_eq!(bad.len(), 2, "{bad:?}");
    assert_eq!(bad[0].code.as_deref(), Some("full_tag"));
    assert_eq!(bad[0].span.map(|s| s.start_line), Some(8));
    assert_eq!(bad[1].code.as_deref(), Some("sha"));
    assert_eq!(bad[1].severity, Severity::Error);
    assert!(results[1].as_ref().expect("ran").is_empty(), "{:?}", results[1]);
}
