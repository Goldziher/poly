//! Unit tests for the `lint` module. Split into a sibling file to keep
//! `lint.rs` under the 1000-line module cap; pulled back in via `#[path]`
//! so these stay in-crate unit tests with access to private items.

#![allow(clippy::field_reassign_with_default)]

use super::*;

#[test]
fn rejects_empty_title() {
    let options = LintOptions::default();
    let outcome = lint_message("", &options);
    assert!(
        outcome
            .violations_before
            .iter()
            .any(|msg| msg.contains("title (first line) must not be empty")),
        "expected empty title violation"
    );
}

#[test]
fn enforces_message_pattern() {
    let pattern = build_message_pattern("^feat: .+$", None).unwrap();
    let mut options = LintOptions::default();
    options.message_pattern = Some(pattern);
    let outcome = lint_message("fix: nope", &options);
    assert_eq!(outcome.violations_before.len(), 1);
}

#[test]
fn applies_cleanup_rules() {
    let cleanup = build_cleanup_rule("\\s+$", "", Some("trim trailing whitespace".into())).unwrap();
    let mut options = LintOptions::default();
    options.cleanup_rules.push(cleanup);
    let outcome = lint_message("feat: demo   \n", &options);
    assert_eq!(outcome.cleaned_message, "feat: demo");
    assert_eq!(outcome.cleanup_summaries.len(), 1);
    assert!(outcome.violations_after.is_empty());
}

#[test]
fn autofix_preserves_single_trailing_newline() {
    let mut options = LintOptions::default();
    options.autofix = true;
    let input = "feat: demo\n";
    let outcome = lint_message(input, &options);
    assert_eq!(outcome.cleaned_message, input);
    assert!(outcome.cleanup_summaries.is_empty());
}

#[test]
fn autofix_trims_excess_trailing_blank_lines() {
    let mut options = LintOptions::default();
    options.autofix = true;
    let outcome = lint_message("feat: demo\n\n\n", &options);
    assert_eq!(outcome.cleaned_message, "feat: demo\n");
    assert!(
        outcome
            .cleanup_summaries
            .iter()
            .any(|msg| msg == "Trim leading/trailing blank lines")
    );
}

#[test]
fn autofix_trims_trailing_whitespace() {
    let mut options = LintOptions::default();
    options.autofix = true;
    let outcome = lint_message("feat: demo  \nbody\t \n", &options);
    assert_eq!(outcome.cleaned_message, "feat: demo\nbody\n");
    assert!(
        outcome
            .cleanup_summaries
            .iter()
            .any(|msg| msg == "Trim trailing whitespace")
    );
}

#[test]
fn autofix_inserts_blank_line_before_body() {
    let mut options = LintOptions::default();
    options.autofix = true;
    options.enforce_conventional_spec = true;
    let outcome = lint_message("feat: add api\nbody line", &options);
    assert_eq!(outcome.cleaned_message, "feat: add api\n\nbody line");
    assert!(outcome.warnings_after.is_empty());
    assert!(
        outcome
            .cleanup_summaries
            .iter()
            .any(|msg| msg == "Insert blank line before body")
    );
}

#[test]
fn autofix_inserts_blank_line_before_footer() {
    let mut options = LintOptions::default();
    options.autofix = true;
    options.enforce_conventional_spec = true;
    let message = "feat: add api\n\nBody line\nRefs: 123";
    let outcome = lint_message(message, &options);
    assert_eq!(
        outcome.cleaned_message,
        "feat: add api\n\nBody line\n\nRefs: 123"
    );
    assert!(outcome.warnings_after.is_empty());
    assert!(
        outcome
            .cleanup_summaries
            .iter()
            .any(|msg| msg == "Insert blank line before footers")
    );
}

#[test]
fn excludes_patterns() {
    let exclude = build_exclude_rule("(?i)wip", Some("WIP commits disallowed".into())).unwrap();
    let mut options = LintOptions::default();
    options.exclude_rules.push(exclude);
    let outcome = lint_message("wip: tmp", &options);
    assert_eq!(outcome.violations_before, vec!["WIP commits disallowed"]);
}

#[test]
fn enforces_single_line_policy() {
    let mut options = LintOptions::default();
    options.body_policy = BodyPolicy::SingleLine;
    let outcome = lint_message("feat: header\n\nbody line", &options);
    assert!(
        outcome
            .violations_before
            .iter()
            .any(|msg| msg.contains("single line"))
    );
}

#[test]
fn enforces_require_body_policy() {
    let mut options = LintOptions::default();
    options.body_policy = BodyPolicy::RequireBody;
    let outcome = lint_message("feat: header\n", &options);
    assert!(
        outcome
            .violations_before
            .iter()
            .any(|msg| msg.contains("must include a body"))
    );

    let ok = lint_message("feat: header\n\nbody", &options);
    assert!(
        ok.violations_before
            .iter()
            .all(|msg| !msg.contains("must include a body"))
    );
}

#[test]
fn conventional_commit_with_body_and_footer_is_valid() {
    let mut options = LintOptions::default();
    options.message_pattern = Some(
        build_message_pattern(
            "^(?P<type>[A-Za-z]+)(\\((?P<scope>[^)]+)\\))?(?P<breaking>!)?: (?P<description>.+)$",
            Some("Conventional".into()),
        )
        .unwrap(),
    );
    options.enforce_conventional_spec = true;
    let message = "feat(parser): support pipes\n\nAdd parsing for foo | bar\n\nRefs: 123";
    let outcome = lint_message(message, &options);
    assert!(
        outcome.violations_before.is_empty(),
        "expected no violations, got {:?}",
        outcome.violations_before
    );
    assert!(
        outcome.warnings_before.is_empty(),
        "expected no warnings, got {:?}",
        outcome.warnings_before
    );
}

#[test]
fn conventional_commit_requires_blank_line_before_body() {
    let mut options = LintOptions::default();
    options.message_pattern = Some(
        build_message_pattern(
            "^(?P<type>[A-Za-z]+)(\\((?P<scope>[^)]+)\\))?(?P<breaking>!)?: (?P<description>.+)$",
            Some("Conventional".into()),
        )
        .unwrap(),
    );
    options.enforce_conventional_spec = true;
    let message = "feat: add api\nbody without separator";
    let outcome = lint_message(message, &options);
    assert!(
        outcome
            .warnings_before
            .iter()
            .any(|msg| msg == "body must have leading blank line"),
        "expected body-leading-blank warning"
    );
}

#[test]
fn footers_require_blank_line() {
    let mut options = LintOptions::default();
    options.message_pattern = Some(
        build_message_pattern(
            "^(?P<type>[A-Za-z]+)(\\((?P<scope>[^)]+)\\))?(?P<breaking>!)?: (?P<description>.+)$",
            Some("Conventional".into()),
        )
        .unwrap(),
    );
    options.enforce_conventional_spec = true;
    let message = "feat: adjust login\nBREAKING CHANGE: password flow updated";
    let outcome = lint_message(message, &options);
    assert!(
        outcome
            .warnings_before
            .iter()
            .any(|msg| msg == "footer must have leading blank line"),
        "expected footer-leading-blank warning"
    );
}

#[test]
fn breaking_change_footer_requires_description() {
    let mut options = LintOptions::default();
    options.message_pattern = Some(
        build_message_pattern(
            "^(?P<type>[A-Za-z]+)(\\((?P<scope>[^)]+)\\))?(?P<breaking>!)?: (?P<description>.+)$",
            Some("Conventional".into()),
        )
        .unwrap(),
    );
    options.enforce_conventional_spec = true;
    let message = "feat!: add api\n\nBREAKING CHANGE: ";
    let outcome = lint_message(message, &options);
    assert!(
        outcome
            .violations_before
            .iter()
            .any(|msg| msg.contains("BREAKING CHANGE footer must include a description")),
        "expected breaking change description violation"
    );
}

#[test]
fn breaking_change_token_must_be_uppercase() {
    let mut options = LintOptions::default();
    options.message_pattern = Some(
        build_message_pattern(
            "^(?P<type>[A-Za-z]+)(\\((?P<scope>[^)]+)\\))?(?P<breaking>!)?: (?P<description>.+)$",
            Some("Conventional".into()),
        )
        .unwrap(),
    );
    options.enforce_conventional_spec = true;
    let message = "feat: add option\n\nbreaking change: not uppercase";
    let outcome = lint_message(message, &options);
    assert!(
        outcome
            .violations_before
            .iter()
            .any(|msg| msg.contains("BREAKING CHANGE footer token must be uppercase")),
        "expected uppercase violation"
    );
}

#[test]
fn conventional_body_allows_bullets_with_colons() {
    let mut options = LintOptions::default();
    options.message_pattern = Some(
        build_message_pattern(
            "^(?P<type>\\w+)(\\((?P<scope>.*)\\))?(?P<breaking>!)?: (?P<description>.+)$",
            Some("Conventional".into()),
        )
        .unwrap(),
    );
    options.enforce_conventional_spec = true;
    let message = "feat: add api\n\n- Update: handle edge cases\n- Note: keep API stable\n\nBREAKING CHANGE: endpoint renamed";
    let outcome = lint_message(message, &options);
    assert!(
        outcome.violations_before.is_empty(),
        "expected no violations, got {:?}",
        outcome.violations_before
    );
}

#[test]
fn conventional_title_allows_digits_and_underscore() {
    let mut options = LintOptions::default();
    options.message_pattern = Some(
        build_message_pattern(
            "^(?P<type>\\w+)(\\((?P<scope>.*)\\))?(?P<breaking>!)?: (?P<description>.+)$",
            Some("Conventional".into()),
        )
        .unwrap(),
    );
    options.enforce_conventional_spec = true;
    let message = "ci(test_2): add workflow caching";
    let outcome = lint_message(message, &options);
    assert!(
        outcome.violations_before.is_empty(),
        "expected no violations, got {:?}",
        outcome.violations_before
    );
}
